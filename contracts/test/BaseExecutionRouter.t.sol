// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test, console} from "forge-std/Test.sol";

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

import {BaseExecutionRouter} from "../src/BaseExecutionRouter.sol";
import {IBalancerV2Vault} from "../src/interfaces/IBalancerV2Vault.sol";

import {MockERC20} from "./mocks/MockERC20.sol";
import {MockBalancerVault} from "./mocks/MockBalancerVault.sol";
import {
    MockUniswapV2Router,
    MockUniswapV3SwapRouter,
    MockAerodromeRouter
} from "./mocks/MockSwapVenues.sol";

contract BaseExecutionRouterTest is Test {
    BaseExecutionRouter internal router;
    MockBalancerVault internal vault;

    MockERC20 internal weth;
    MockERC20 internal usdc;

    MockUniswapV3SwapRouter internal v3Router;
    MockUniswapV2Router internal v2Router;
    MockAerodromeRouter internal aerodromeRouter;

    address internal owner = address(0xA11CE);
    address internal executor = address(0xB0B);
    address internal aerodromeFactory = address(0xFAC7);

    uint256 internal constant LOAN = 1_000_000 ether;

    function setUp() public {
        vault = new MockBalancerVault();
        vm.prank(owner);
        router = new BaseExecutionRouter(owner, IBalancerV2Vault(address(vault)));

        vm.prank(owner);
        router.setExecutor(executor, true);

        weth = new MockERC20("Wrapped Ether", "WETH", 18);
        usdc = new MockERC20("USD Coin", "USDC", 6);

        v3Router = new MockUniswapV3SwapRouter();
        v2Router = new MockUniswapV2Router();
        aerodromeRouter = new MockAerodromeRouter();

        // Seed the venues so they can deliver `tokenOut` on swap.
        usdc.mint(address(v3Router), 10_000_000_000_000);
        weth.mint(address(v3Router), 10_000_000 ether);
        usdc.mint(address(v2Router), 10_000_000_000_000);
        weth.mint(address(v2Router), 10_000_000 ether);
        usdc.mint(address(aerodromeRouter), 10_000_000_000_000);
        weth.mint(address(aerodromeRouter), 10_000_000 ether);

        // Seed the vault with WETH inventory used for flash loans.
        weth.mint(address(vault), 10_000_000 ether);
    }

    // ------------------------------------------------------------------
    // executePlan (self-funded)
    // ------------------------------------------------------------------

    function test_selfFunded_two_leg_v2_v3_route_returns_surplus_to_caller() public {
        v2Router.setOutput(2_000_000_000_000); // WETH -> USDC
        v3Router.setOutput(LOAN + 5 ether); // USDC -> WETH (5 wei surplus over loan)

        weth.mint(executor, LOAN);
        vm.prank(executor);
        weth.approve(address(router), LOAN);

        BaseExecutionRouter.ExecutionPlan memory plan = _twoLegPlan(LOAN, LOAN, 1);

        vm.prank(executor);
        uint256 amountOut = router.executePlan(plan);

        assertEq(amountOut, LOAN + 5 ether);
        assertEq(weth.balanceOf(executor), LOAN + 5 ether);
        assertEq(weth.balanceOf(address(router)), 0);
    }

    function test_selfFunded_reverts_when_step_minimum_is_violated() public {
        v2Router.setOutput(2_000_000_000_000);
        v3Router.setOutput(LOAN); // exactly the loan, fails minSurplus

        weth.mint(executor, LOAN);
        vm.prank(executor);
        weth.approve(address(router), LOAN);

        BaseExecutionRouter.ExecutionPlan memory plan = _twoLegPlan(LOAN, LOAN, 1);

        vm.prank(executor);
        vm.expectRevert(
            abi.encodeWithSelector(
                BaseExecutionRouter.InsufficientSettlement.selector,
                LOAN,
                LOAN + 1
            )
        );
        router.executePlan(plan);
    }

    function test_selfFunded_reverts_when_per_step_minimum_is_violated() public {
        v2Router.setOutput(1); // way below the per-step minimum
        v3Router.setOutput(LOAN + 5 ether);

        weth.mint(executor, LOAN);
        vm.prank(executor);
        weth.approve(address(router), LOAN);

        BaseExecutionRouter.ExecutionPlan memory plan = _twoLegPlan(LOAN, LOAN, 1);
        plan.steps[0].minAmountOut = 1_000_000_000_000; // require 1M USDC

        vm.prank(executor);
        vm.expectRevert(
            abi.encodeWithSelector(
                BaseExecutionRouter.InsufficientStepOutput.selector, 0, 1, 1_000_000_000_000
            )
        );
        router.executePlan(plan);
    }

    function test_selfFunded_reverts_when_caller_is_not_executor() public {
        BaseExecutionRouter.ExecutionPlan memory plan = _twoLegPlan(LOAN, LOAN, 1);

        vm.expectRevert(BaseExecutionRouter.ExecutorNotAuthorized.selector);
        router.executePlan(plan);
    }

    function test_selfFunded_reverts_when_paused() public {
        vm.prank(owner);
        router.pause();

        weth.mint(executor, LOAN);
        vm.prank(executor);
        weth.approve(address(router), LOAN);
        BaseExecutionRouter.ExecutionPlan memory plan = _twoLegPlan(LOAN, LOAN, 1);

        vm.prank(executor);
        vm.expectRevert(); // OZ Pausable error type changes across versions; selector-agnostic
        router.executePlan(plan);
    }

    function test_selfFunded_reverts_when_deadline_passed() public {
        weth.mint(executor, LOAN);
        vm.prank(executor);
        weth.approve(address(router), LOAN);

        BaseExecutionRouter.ExecutionPlan memory plan = _twoLegPlan(LOAN, LOAN, 1);
        plan.deadline = block.timestamp - 1;

        vm.prank(executor);
        vm.expectRevert(
            abi.encodeWithSelector(
                BaseExecutionRouter.PlanExpired.selector, plan.deadline, block.timestamp
            )
        );
        router.executePlan(plan);
    }

    // ------------------------------------------------------------------
    // executePlanWithFlashLoan
    // ------------------------------------------------------------------

    function test_flashLoan_round_trip_repays_vault_and_pays_caller_surplus() public {
        v2Router.setOutput(2_000_000_000_000);
        v3Router.setOutput(LOAN + 7 ether);

        BaseExecutionRouter.ExecutionPlan memory plan = _twoLegPlan(LOAN, LOAN, 1);

        uint256 vaultPre = weth.balanceOf(address(vault));
        uint256 callerPre = weth.balanceOf(executor);

        vm.prank(executor);
        router.executePlanWithFlashLoan(plan);

        assertEq(weth.balanceOf(address(vault)), vaultPre, "vault balance must be intact");
        assertEq(weth.balanceOf(executor), callerPre + 7 ether);
        assertEq(weth.balanceOf(address(router)), 0);
    }

    function test_flashLoan_reverts_when_route_is_underwater() public {
        v2Router.setOutput(2_000_000_000_000);
        v3Router.setOutput(LOAN); // can't repay loan + surplus

        BaseExecutionRouter.ExecutionPlan memory plan = _twoLegPlan(LOAN, LOAN, 1);

        vm.prank(executor);
        vm.expectRevert(
            abi.encodeWithSelector(
                BaseExecutionRouter.InsufficientSettlement.selector, LOAN, LOAN + 1
            )
        );
        router.executePlanWithFlashLoan(plan);
    }

    function test_flashLoan_reverts_when_vault_charges_fee() public {
        vault.setFlashLoanFee(address(weth), 1);
        v2Router.setOutput(2_000_000_000_000);
        v3Router.setOutput(LOAN + 7 ether);

        BaseExecutionRouter.ExecutionPlan memory plan = _twoLegPlan(LOAN, LOAN, 1);

        vm.prank(executor);
        vm.expectRevert(abi.encodeWithSelector(BaseExecutionRouter.UnexpectedFlashLoanFee.selector, 1));
        router.executePlanWithFlashLoan(plan);
    }

    function test_flashLoan_callback_rejects_unknown_caller() public {
        // Direct invocation outside of a vault-initiated flash loan must revert.
        IERC20[] memory tokens = new IERC20[](1);
        uint256[] memory amounts = new uint256[](1);
        uint256[] memory feeAmounts = new uint256[](1);
        tokens[0] = IERC20(address(weth));
        amounts[0] = LOAN;
        feeAmounts[0] = 0;

        vm.expectRevert(
            abi.encodeWithSelector(
                BaseExecutionRouter.UnexpectedFlashLoanCaller.selector, address(this)
            )
        );
        router.receiveFlashLoan(tokens, amounts, feeAmounts, "");
    }

    // ------------------------------------------------------------------
    // Aerodrome leg coverage
    // ------------------------------------------------------------------

    function test_three_leg_route_via_aerodrome_succeeds() public {
        v3Router.setOutput(2_000_000_000_000); // WETH -> USDC
        aerodromeRouter.setOutput(1_999_000_000_000); // USDC -> USDC' (assume same token for mock)
        v2Router.setOutput(LOAN + 3 ether); // USDC' -> WETH

        weth.mint(executor, LOAN);
        vm.prank(executor);
        weth.approve(address(router), LOAN);

        BaseExecutionRouter.ExecutionPlan memory plan = _threeLegPlanV3AeroV2(LOAN);

        vm.prank(executor);
        uint256 amountOut = router.executePlan(plan);
        assertEq(amountOut, LOAN + 3 ether);
    }

    // ------------------------------------------------------------------
    // Admin
    // ------------------------------------------------------------------

    function test_only_owner_can_set_executor() public {
        vm.expectRevert();
        router.setExecutor(address(0xCAFE), true);
    }

    function test_rescue_token_returns_balance_to_recipient() public {
        weth.mint(address(router), 5 ether);
        address recipient = address(0xDEADBEEF);

        vm.prank(owner);
        router.rescueToken(address(weth), recipient, 5 ether);
        assertEq(weth.balanceOf(recipient), 5 ether);
    }

    // ------------------------------------------------------------------
    // helpers
    // ------------------------------------------------------------------

    function _twoLegPlan(uint256 funding, uint256 minRepay, uint256 minSurplus)
        internal
        view
        returns (BaseExecutionRouter.ExecutionPlan memory plan)
    {
        BaseExecutionRouter.RouteStep[] memory steps = new BaseExecutionRouter.RouteStep[](2);
        steps[0] = BaseExecutionRouter.RouteStep({
            venueKind: BaseExecutionRouter.VenueKind.UniswapV2,
            target: address(v2Router),
            tokenIn: address(weth),
            tokenOut: address(usdc),
            fee: 0,
            minAmountOut: 1,
            extraData: ""
        });
        steps[1] = BaseExecutionRouter.RouteStep({
            venueKind: BaseExecutionRouter.VenueKind.UniswapV3Single,
            target: address(v3Router),
            tokenIn: address(usdc),
            tokenOut: address(weth),
            fee: 500,
            minAmountOut: 1,
            extraData: ""
        });

        plan = BaseExecutionRouter.ExecutionPlan({
            settlementToken: address(weth),
            fundingAmount: funding,
            minRepayAmount: minRepay,
            minSurplus: minSurplus,
            deadline: block.timestamp + 60,
            steps: steps,
            riskHash: keccak256("risk")
        });
    }

    function _threeLegPlanV3AeroV2(uint256 funding)
        internal
        view
        returns (BaseExecutionRouter.ExecutionPlan memory plan)
    {
        BaseExecutionRouter.RouteStep[] memory steps = new BaseExecutionRouter.RouteStep[](3);
        steps[0] = BaseExecutionRouter.RouteStep({
            venueKind: BaseExecutionRouter.VenueKind.UniswapV3Single,
            target: address(v3Router),
            tokenIn: address(weth),
            tokenOut: address(usdc),
            fee: 500,
            minAmountOut: 1,
            extraData: ""
        });
        steps[1] = BaseExecutionRouter.RouteStep({
            venueKind: BaseExecutionRouter.VenueKind.AerodromeV2,
            target: address(aerodromeRouter),
            tokenIn: address(usdc),
            tokenOut: address(usdc),
            fee: 0,
            minAmountOut: 1,
            extraData: abi.encode(true, aerodromeFactory)
        });
        steps[2] = BaseExecutionRouter.RouteStep({
            venueKind: BaseExecutionRouter.VenueKind.UniswapV2,
            target: address(v2Router),
            tokenIn: address(usdc),
            tokenOut: address(weth),
            fee: 0,
            minAmountOut: 1,
            extraData: ""
        });

        plan = BaseExecutionRouter.ExecutionPlan({
            settlementToken: address(weth),
            fundingAmount: funding,
            minRepayAmount: funding,
            minSurplus: 1,
            deadline: block.timestamp + 60,
            steps: steps,
            riskHash: keccak256("risk-3")
        });
    }
}
