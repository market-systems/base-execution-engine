// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

import {IUniswapV2Router} from "../../src/interfaces/IUniswapV2Router.sol";
import {IUniswapV3SwapRouter} from "../../src/interfaces/IUniswapV3SwapRouter.sol";
import {IAerodromeRouter} from "../../src/interfaces/IAerodromeRouter.sol";

/// @notice Test double that returns a configurable amountOut for any swap. The
/// mock pulls `amountIn` of `tokenIn` from the caller and mints/transfers
/// `outputAmount` of `tokenOut`, ignoring price discovery entirely. The point is
/// to exercise the router's accounting and slippage logic, not pool math.
abstract contract MockSwapVenue {
    uint256 public outputAmount;
    bool public shouldRevert;

    function setOutput(uint256 amount) external {
        outputAmount = amount;
    }

    function setShouldRevert(bool flag) external {
        shouldRevert = flag;
    }

    function _settle(address tokenIn, address tokenOut, uint256 amountIn, address recipient)
        internal
        returns (uint256)
    {
        require(!shouldRevert, "MockSwapVenue: forced revert");
        IERC20(tokenIn).transferFrom(msg.sender, address(this), amountIn);
        uint256 out = outputAmount;
        IERC20(tokenOut).transfer(recipient, out);
        return out;
    }
}

contract MockUniswapV2Router is MockSwapVenue, IUniswapV2Router {
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 /* amountOutMin */,
        address[] calldata path,
        address to,
        uint256 /* deadline */
    ) external override returns (uint256[] memory amounts) {
        uint256 out = _settle(path[0], path[path.length - 1], amountIn, to);
        amounts = new uint256[](path.length);
        amounts[0] = amountIn;
        amounts[path.length - 1] = out;
    }
}

contract MockUniswapV3SwapRouter is MockSwapVenue, IUniswapV3SwapRouter {
    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        override
        returns (uint256 amountOut)
    {
        amountOut = _settle(params.tokenIn, params.tokenOut, params.amountIn, params.recipient);
    }
}

contract MockAerodromeRouter is MockSwapVenue, IAerodromeRouter {
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 /* amountOutMin */,
        Route[] calldata routes,
        address to,
        uint256 /* deadline */
    ) external override returns (uint256[] memory amounts) {
        Route calldata first = routes[0];
        Route calldata last = routes[routes.length - 1];
        uint256 out = _settle(first.from, last.to, amountIn, to);
        amounts = new uint256[](routes.length + 1);
        amounts[0] = amountIn;
        amounts[routes.length] = out;
    }
}
