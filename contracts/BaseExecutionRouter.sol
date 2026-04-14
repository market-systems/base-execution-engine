// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "openzeppelin-contracts/contracts/access/Ownable.sol";
import "openzeppelin-contracts/contracts/security/Pausable.sol";
import "openzeppelin-contracts/contracts/token/ERC20/IERC20.sol";
import "openzeppelin-contracts/contracts/token/ERC20/utils/SafeERC20.sol";

interface ISwapRouterV3 {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(
        ExactInputSingleParams calldata params
    ) external payable returns (uint256 amountOut);
}

contract BaseExecutionRouter is Ownable, Pausable {
    using SafeERC20 for IERC20;

    enum VenueKind {
        Unsupported,
        UniswapV3Single
    }

    struct RouteStep {
        VenueKind venueKind;
        address target;
        address tokenIn;
        address tokenOut;
        uint24 fee;
        bytes extraData;
    }

    struct ExecutionPlan {
        address fundingToken;
        uint256 amountIn;
        uint256 minAmountOut;
        RouteStep[] steps;
        bytes32 riskHash;
    }

    mapping(address => bool) public executors;

    event ExecutorUpdated(address indexed executor, bool allowed);
    event PlanExecuted(
        address indexed caller,
        address indexed fundingToken,
        uint256 amountIn,
        uint256 amountOut,
        bytes32 riskHash
    );

    error ExecutorNotAuthorized();
    error UnsupportedVenue();
    error InsufficientOutput(uint256 actualAmountOut, uint256 minimumAmountOut);

    modifier onlyExecutor() {
        if (!executors[msg.sender] && msg.sender != owner()) {
            revert ExecutorNotAuthorized();
        }
        _;
    }

    constructor() Ownable(msg.sender) {}

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

    function executePlan(
        ExecutionPlan calldata plan
    ) external payable onlyExecutor whenNotPaused returns (uint256 amountOut) {
        IERC20(plan.fundingToken).safeTransferFrom(
            msg.sender,
            address(this),
            plan.amountIn
        );

        uint256 currentAmount = plan.amountIn;
        address currentToken = plan.fundingToken;

        for (uint256 index = 0; index < plan.steps.length; index++) {
            RouteStep calldata step = plan.steps[index];

            if (step.venueKind != VenueKind.UniswapV3Single) {
                revert UnsupportedVenue();
            }

            if (step.tokenIn != currentToken) {
                revert UnsupportedVenue();
            }

            IERC20(step.tokenIn).forceApprove(step.target, currentAmount);

            ISwapRouterV3.ExactInputSingleParams memory params = ISwapRouterV3
                .ExactInputSingleParams({
                    tokenIn: step.tokenIn,
                    tokenOut: step.tokenOut,
                    fee: step.fee,
                    recipient: address(this),
                    deadline: block.timestamp,
                    amountIn: currentAmount,
                    amountOutMinimum: 0,
                    sqrtPriceLimitX96: 0
                });

            currentAmount = ISwapRouterV3(step.target).exactInputSingle(params);
            currentToken = step.tokenOut;
        }

        amountOut = currentAmount;
        if (amountOut < plan.minAmountOut) {
            revert InsufficientOutput(amountOut, plan.minAmountOut);
        }

        IERC20(currentToken).safeTransfer(msg.sender, amountOut);
        emit PlanExecuted(
            msg.sender,
            plan.fundingToken,
            plan.amountIn,
            amountOut,
            plan.riskHash
        );
    }

    function rescueToken(address token, address recipient) external onlyOwner {
        IERC20 erc20 = IERC20(token);
        erc20.safeTransfer(recipient, erc20.balanceOf(address(this)));
    }
}
