// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

import {IBalancerV2Vault, IFlashLoanRecipient} from "../../src/interfaces/IBalancerV2Vault.sol";

/// @notice Faithful test double for the Balancer V2 vault flash-loan API.
/// Transfers tokens to the recipient, calls back, then asserts that exactly
/// `amount + fee` was returned. The fee is configurable per token to mirror
/// the production "0% on Base" assumption while keeping the option to test
/// fee-bearing scenarios (and verify the executor reverts on them).
contract MockBalancerVault is IBalancerV2Vault {
    mapping(address => uint256) public flashLoanFeePerToken;

    error InvalidPostBalance(address token, uint256 expected, uint256 actual);

    function setFlashLoanFee(address token, uint256 fee) external {
        flashLoanFeePerToken[token] = fee;
    }

    function flashLoan(
        IFlashLoanRecipient recipient,
        IERC20[] memory tokens,
        uint256[] memory amounts,
        bytes memory userData
    ) external override {
        uint256 length = tokens.length;
        uint256[] memory feeAmounts = new uint256[](length);
        uint256[] memory preBalances = new uint256[](length);

        for (uint256 i = 0; i < length; i++) {
            feeAmounts[i] = flashLoanFeePerToken[address(tokens[i])];
            preBalances[i] = tokens[i].balanceOf(address(this));
            tokens[i].transfer(address(recipient), amounts[i]);
        }

        recipient.receiveFlashLoan(tokens, amounts, feeAmounts, userData);

        for (uint256 i = 0; i < length; i++) {
            uint256 expected = preBalances[i] + feeAmounts[i];
            uint256 actual = tokens[i].balanceOf(address(this));
            if (actual < expected) {
                revert InvalidPostBalance(address(tokens[i]), expected, actual);
            }
        }
    }
}
