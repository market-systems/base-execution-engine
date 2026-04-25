// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @notice Subset of the Aerodrome (Solidly fork) router used for volatile/stable pool swaps.
interface IAerodromeRouter {
    struct Route {
        address from;
        address to;
        bool stable;
        address factory;
    }

    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        Route[] calldata routes,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);
}
