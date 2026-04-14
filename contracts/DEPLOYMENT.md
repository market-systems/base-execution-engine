# BaseExecutionRouter Deployment

This document describes the current deployment flow for
`contracts/BaseExecutionRouter.sol`.

## Requirements

- Foundry installed
- A funded Base account for deployment
- A Base RPC endpoint
- The operator address that will be allowed to execute plans

## Environment

```bash
export BASE_RPC_URL="https://mainnet.base.org"
export DEPLOYER_PRIVATE_KEY="<deployer-private-key>"
export EXECUTOR_ADDRESS="<executor-address>"
export EXECUTOR_PRIVATE_KEY="<executor-private-key>"
```

## Deploy

```bash
forge create \
  --rpc-url "$BASE_RPC_URL" \
  --private-key "$DEPLOYER_PRIVATE_KEY" \
  contracts/BaseExecutionRouter.sol:BaseExecutionRouter
```

Record the deployed contract address after the command succeeds.

## Authorize An Executor

`BaseExecutionRouter` requires the caller to be either the owner or an approved
executor.

```bash
cast send <router-address> \
  "setExecutor(address,bool)" \
  "$EXECUTOR_ADDRESS" \
  true \
  --rpc-url "$BASE_RPC_URL" \
  --private-key "$DEPLOYER_PRIVATE_KEY"
```

## Pause And Unpause

```bash
cast send <router-address> \
  "pause()" \
  --rpc-url "$BASE_RPC_URL" \
  --private-key "$DEPLOYER_PRIVATE_KEY"
```

```bash
cast send <router-address> \
  "unpause()" \
  --rpc-url "$BASE_RPC_URL" \
  --private-key "$DEPLOYER_PRIVATE_KEY"
```

## Funding Token Approval

The router pulls the funding token from the caller with `transferFrom`, so the
executor must approve the router before calling `executePlan`.

For WETH on Base:

```bash
cast send 0x4200000000000000000000000000000000000006 \
  "approve(address,uint256)" \
  <router-address> \
  115792089237316195423570985008687907853269984665640564039457584007913129639935 \
  --rpc-url "$BASE_RPC_URL" \
  --private-key "$EXECUTOR_PRIVATE_KEY"
```

## Notes

- The current contract supports a constrained execution model.
- Route encoding must stay aligned with the Rust-side execution plan model.
- Use the executor key for token approval if the executor is different from the
  deployer.
- Review the contract source before using it in live mode.
