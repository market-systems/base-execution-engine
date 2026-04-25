// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

import {IBalancerV2Vault, IFlashLoanRecipient} from "./interfaces/IBalancerV2Vault.sol";
import {IUniswapV2Router} from "./interfaces/IUniswapV2Router.sol";
import {IUniswapV3SwapRouter} from "./interfaces/IUniswapV3SwapRouter.sol";
import {IAerodromeRouter} from "./interfaces/IAerodromeRouter.sol";

/// @title BaseExecutionRouter
/// @notice Atomic multi-venue arbitrage executor for Base L2.
/// @dev
/// - Two entry points: self-funded `executePlan` and Balancer V2 flash-loan-funded
///   `executePlanWithFlashLoan`. Both share a single `_runRoute` validator that
///   enforces per-step `minAmountOut`, the route ending in `settlementToken`, and
///   the global `minRepayAmount + minSurplus` invariant.
/// - Balancer V2 flash loans charge zero fee on Base; the contract still
///   defensively asserts `feeAmounts == 0` so that a future fee schedule cannot
///   silently bleed inventory.
/// - Reentrancy is blocked on every external entry point. The Balancer callback
///   is gated on a transient `_loanContext` slot so only an in-flight plan can
///   accept a `receiveFlashLoan` call from the vault.
contract BaseExecutionRouter is Ownable, Pausable, ReentrancyGuard, IFlashLoanRecipient {
    using SafeERC20 for IERC20;

    /// @notice Supported venue kinds. The numeric value is part of the executor ABI;
    /// new venues MUST be appended at the end to keep client encodings stable.
    enum VenueKind {
        Unsupported,
        UniswapV2,
        UniswapV3Single,
        AerodromeV2
    }

    /// @notice One leg of the route. `extraData` is venue-specific:
    /// - UniswapV2 / UniswapV3Single: empty
    /// - AerodromeV2: abi.encode(bool stable, address factory)
    struct RouteStep {
        VenueKind venueKind;
        address target;
        address tokenIn;
        address tokenOut;
        uint24 fee;
        uint256 minAmountOut;
        bytes extraData;
    }

    /// @notice Full execution plan submitted by the off-chain decision engine.
    struct ExecutionPlan {
        address settlementToken;
        uint256 fundingAmount;
        uint256 minRepayAmount;
        uint256 minSurplus;
        uint256 deadline;
        RouteStep[] steps;
        bytes32 riskHash;
    }

    IBalancerV2Vault public immutable balancerVault;

    mapping(address => bool) public executors;

    /// @dev Sentinel for an in-flight Balancer flash loan. Holds the keccak hash
    /// of `abi.encode(caller, plan)` while the loan is active and is reset to
    /// `bytes32(0)` immediately after the vault returns.
    bytes32 private _activeLoanFingerprint;

    event ExecutorUpdated(address indexed executor, bool allowed);
    event PlanExecuted(
        address indexed caller,
        address indexed settlementToken,
        uint256 fundingAmount,
        uint256 amountOut,
        uint256 surplus,
        bool flashLoaned,
        bytes32 riskHash
    );
    event TokenRescued(address indexed token, address indexed recipient, uint256 amount);

    error ExecutorNotAuthorized();
    error PlanExpired(uint256 deadline, uint256 nowTimestamp);
    error EmptyRoute();
    error UnsupportedVenue(VenueKind venueKind);
    error InvalidRouteToken(uint256 stepIndex, address expected, address actual);
    error InsufficientStepOutput(uint256 stepIndex, uint256 amountOut, uint256 minAmountOut);
    error InsufficientSettlement(uint256 amountOut, uint256 minimumAmountOut);
    error UnexpectedFlashLoanCaller(address caller);
    error UnexpectedFlashLoanFee(uint256 fee);
    error UnexpectedFlashLoanToken(address expected, address actual);
    error UnexpectedFlashLoanAmount(uint256 expected, uint256 actual);
    error UnexpectedFlashLoanArrayLength();
    error LoanFingerprintMismatch(bytes32 expected, bytes32 actual);
    error FlashLoanContextActive();

    modifier onlyExecutor() {
        if (!executors[msg.sender] && msg.sender != owner()) {
            revert ExecutorNotAuthorized();
        }
        _;
    }

    constructor(address initialOwner, IBalancerV2Vault vault) Ownable(initialOwner) {
        balancerVault = vault;
    }

    // -----------------------------------------------------------------------
    // Admin
    // -----------------------------------------------------------------------

    function setExecutor(address executor, bool allowed) external onlyOwner {
        executors[executor] = allowed;
        emit ExecutorUpdated(executor, allowed);
    }

    function pause() external onlyOwner {
        _pause();
    }

    function unpause() external onlyOwner {
        _unpause();
    }

    function rescueToken(address token, address recipient, uint256 amount) external onlyOwner {
        IERC20(token).safeTransfer(recipient, amount);
        emit TokenRescued(token, recipient, amount);
    }

    // -----------------------------------------------------------------------
    // Self-funded execution
    // -----------------------------------------------------------------------

    /// @notice Pull `plan.fundingAmount` of `plan.settlementToken` from the caller,
    /// run the route, and return everything plus the surplus to the caller.
    function executePlan(ExecutionPlan calldata plan)
        external
        onlyExecutor
        whenNotPaused
        nonReentrant
        returns (uint256 amountOut)
    {
        _checkDeadline(plan.deadline);
        if (_activeLoanFingerprint != bytes32(0)) revert FlashLoanContextActive();

        IERC20 settlement = IERC20(plan.settlementToken);
        settlement.safeTransferFrom(msg.sender, address(this), plan.fundingAmount);

        amountOut = _runRoute(plan, plan.fundingAmount);

        uint256 minimumSettlement = plan.minRepayAmount + plan.minSurplus;
        if (amountOut < minimumSettlement) {
            revert InsufficientSettlement(amountOut, minimumSettlement);
        }

        settlement.safeTransfer(msg.sender, amountOut);

        emit PlanExecuted(
            msg.sender,
            plan.settlementToken,
            plan.fundingAmount,
            amountOut,
            amountOut - plan.minRepayAmount,
            false,
            plan.riskHash
        );
    }

    // -----------------------------------------------------------------------
    // Flash-loan-backed execution (Balancer V2)
    // -----------------------------------------------------------------------

    /// @notice Borrow `plan.fundingAmount` of `plan.settlementToken` from Balancer
    /// V2, run the route, repay the loan, and forward any surplus to the caller.
    function executePlanWithFlashLoan(ExecutionPlan calldata plan)
        external
        onlyExecutor
        whenNotPaused
        nonReentrant
    {
        _checkDeadline(plan.deadline);
        if (_activeLoanFingerprint != bytes32(0)) revert FlashLoanContextActive();

        bytes memory userData = abi.encode(msg.sender, plan);
        bytes32 fingerprint = keccak256(userData);
        _activeLoanFingerprint = fingerprint;

        IERC20[] memory tokens = new IERC20[](1);
        uint256[] memory amounts = new uint256[](1);
        tokens[0] = IERC20(plan.settlementToken);
        amounts[0] = plan.fundingAmount;

        balancerVault.flashLoan(IFlashLoanRecipient(address(this)), tokens, amounts, userData);

        // The fingerprint MUST have been cleared by the callback. If it is still
        // set, the vault returned without our callback firing, which means the
        // loan accounting was never closed; abort the whole call.
        if (_activeLoanFingerprint != bytes32(0)) {
            _activeLoanFingerprint = bytes32(0);
            revert LoanFingerprintMismatch(bytes32(0), fingerprint);
        }
    }

    /// @inheritdoc IFlashLoanRecipient
    function receiveFlashLoan(
        IERC20[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external override {
        if (msg.sender != address(balancerVault)) revert UnexpectedFlashLoanCaller(msg.sender);

        bytes32 expectedFingerprint = _activeLoanFingerprint;
        bytes32 fingerprint = keccak256(userData);
        if (expectedFingerprint == bytes32(0) || expectedFingerprint != fingerprint) {
            revert LoanFingerprintMismatch(expectedFingerprint, fingerprint);
        }
        // Clear before doing any external calls so that re-entry through the
        // callback can never re-use the same loan context.
        _activeLoanFingerprint = bytes32(0);

        (address loanCaller, ExecutionPlan memory plan) = abi.decode(userData, (address, ExecutionPlan));

        if (tokens.length != 1 || amounts.length != 1 || feeAmounts.length != 1) {
            revert UnexpectedFlashLoanArrayLength();
        }
        if (address(tokens[0]) != plan.settlementToken) {
            revert UnexpectedFlashLoanToken(plan.settlementToken, address(tokens[0]));
        }
        if (amounts[0] != plan.fundingAmount) {
            revert UnexpectedFlashLoanAmount(plan.fundingAmount, amounts[0]);
        }
        if (feeAmounts[0] != 0) revert UnexpectedFlashLoanFee(feeAmounts[0]);

        uint256 amountOut = _runRoute(plan, plan.fundingAmount);

        uint256 minimumSettlement = plan.minRepayAmount + plan.minSurplus;
        if (amountOut < minimumSettlement) {
            revert InsufficientSettlement(amountOut, minimumSettlement);
        }

        tokens[0].safeTransfer(address(balancerVault), plan.fundingAmount);

        uint256 surplus = amountOut - plan.fundingAmount;
        if (surplus > 0) {
            tokens[0].safeTransfer(loanCaller, surplus);
        }

        emit PlanExecuted(
            loanCaller,
            plan.settlementToken,
            plan.fundingAmount,
            amountOut,
            surplus,
            true,
            plan.riskHash
        );
    }

    // -----------------------------------------------------------------------
    // Internal route engine
    // -----------------------------------------------------------------------

    function _runRoute(ExecutionPlan memory plan, uint256 startingAmount)
        internal
        returns (uint256 amountOut)
    {
        if (plan.steps.length == 0) revert EmptyRoute();

        uint256 currentAmount = startingAmount;
        address currentToken = plan.settlementToken;

        for (uint256 i = 0; i < plan.steps.length; i++) {
            RouteStep memory step = plan.steps[i];

            if (step.tokenIn != currentToken) {
                revert InvalidRouteToken(i, currentToken, step.tokenIn);
            }

            uint256 nextAmount = _executeStep(step, currentAmount, plan.deadline);

            if (nextAmount < step.minAmountOut) {
                revert InsufficientStepOutput(i, nextAmount, step.minAmountOut);
            }

            currentAmount = nextAmount;
            currentToken = step.tokenOut;
        }

        if (currentToken != plan.settlementToken) {
            revert InvalidRouteToken(plan.steps.length, plan.settlementToken, currentToken);
        }

        amountOut = currentAmount;
    }

    function _executeStep(RouteStep memory step, uint256 amountIn, uint256 deadline)
        internal
        returns (uint256)
    {
        if (step.venueKind == VenueKind.UniswapV3Single) {
            return _swapUniswapV3Single(step, amountIn, deadline);
        }
        if (step.venueKind == VenueKind.UniswapV2) {
            return _swapUniswapV2(step, amountIn, deadline);
        }
        if (step.venueKind == VenueKind.AerodromeV2) {
            return _swapAerodromeV2(step, amountIn, deadline);
        }
        revert UnsupportedVenue(step.venueKind);
    }

    function _swapUniswapV3Single(RouteStep memory step, uint256 amountIn, uint256 deadline)
        internal
        returns (uint256)
    {
        IERC20(step.tokenIn).forceApprove(step.target, amountIn);

        IUniswapV3SwapRouter.ExactInputSingleParams memory params = IUniswapV3SwapRouter
            .ExactInputSingleParams({
            tokenIn: step.tokenIn,
            tokenOut: step.tokenOut,
            fee: step.fee,
            recipient: address(this),
            deadline: deadline,
            amountIn: amountIn,
            amountOutMinimum: step.minAmountOut,
            sqrtPriceLimitX96: 0
        });

        return IUniswapV3SwapRouter(step.target).exactInputSingle(params);
    }

    function _swapUniswapV2(RouteStep memory step, uint256 amountIn, uint256 deadline)
        internal
        returns (uint256)
    {
        IERC20(step.tokenIn).forceApprove(step.target, amountIn);

        address[] memory path = new address[](2);
        path[0] = step.tokenIn;
        path[1] = step.tokenOut;

        uint256[] memory amounts = IUniswapV2Router(step.target).swapExactTokensForTokens(
            amountIn, step.minAmountOut, path, address(this), deadline
        );

        return amounts[amounts.length - 1];
    }

    function _swapAerodromeV2(RouteStep memory step, uint256 amountIn, uint256 deadline)
        internal
        returns (uint256)
    {
        (bool stable, address factory) = abi.decode(step.extraData, (bool, address));

        IERC20(step.tokenIn).forceApprove(step.target, amountIn);

        IAerodromeRouter.Route[] memory routes = new IAerodromeRouter.Route[](1);
        routes[0] = IAerodromeRouter.Route({
            from: step.tokenIn,
            to: step.tokenOut,
            stable: stable,
            factory: factory
        });

        uint256[] memory amounts = IAerodromeRouter(step.target).swapExactTokensForTokens(
            amountIn, step.minAmountOut, routes, address(this), deadline
        );

        return amounts[amounts.length - 1];
    }

    function _checkDeadline(uint256 deadline) internal view {
        if (deadline < block.timestamp) revert PlanExpired(deadline, block.timestamp);
    }
}
