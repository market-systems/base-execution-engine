# Base Execution Engine

Low-latency onchain execution engine for the Base L2. The runtime ingests
mempool / Flashblocks / log streams, maintains live pool state, scores
opportunities, runs risk + simulation, and submits atomic multi-venue
arbitrage transactions through a self-deployed router that supports Balancer
V2 flash loans, UniswapV2, UniswapV3, and Aerodrome V2 venues.

## Scope

The system is built for execution workloads that depend on both
pre-confirmation signals and canonical chain state. The intended runtime
shape is:

- ingest pre-confirmation transactions, logs, and Flashblocks pre-confirms
- normalize external payloads into a single internal `Event` stream
- maintain an engine-owned runtime view of market state (V2 reserves, V3
  ticks/sqrtPrice) updated from `Swap`/`Sync` logs
- detect and rank actionable two-leg loop opportunities
- locally simulate quote math, then optionally re-validate via slow-path
  `eth_call`
- enforce risk checks (per-block / per-day notional, slippage ceiling, token
  allow / deny, kill-switch sentinel, consecutive-revert circuit breaker)
- build per-venue calldata and broadcast EIP-1559 transactions through the
  router
- track receipts, classify final outcomes, and emit business metrics

## Quickstart

### Prerequisites

- Rust 1.83+ (`rustup install stable`)
- Foundry (`curl -L https://foundry.paradigm.xyz | bash`)
- Postgres 14+ (or Docker)
- A Base node providing HTTP + WS (or IPC), e.g. local `geth` / `reth`
- Optional: Flashblocks WebSocket access (`wss://mainnet-preconf.base.org`)

### 1. Clone & build

```bash
git clone <repo-url>
cd base-execution-engine
cargo build --workspace
cargo test  --workspace          # 94 unit/integration tests
forge test --root contracts      # router + flash-loan + venue adapter tests
```

### 2. Database bootstrap (run once, as superuser)

```bash
psql "postgres://postgres@HOST:5432/postgres" -f migration/~privileges.sql
```

This creates the `execution_engine` database, the read-write role
`execution_engine`, and the read-only role `execution_engine_ro` (Grafana /
analytics). The engine itself NEVER auto-runs this file because it requires
`CREATEDB / CREATEROLE` privileges that the runtime role must not hold.

The schema migration `migration/00000000-00-init.sql` is applied
automatically on every engine boot when `STORAGE_AUTO_MIGRATE=true`.

### 3. Run the engine

Set the minimum env vars:

```bash
export EXECUTION_MODE=shadow                 # safe default; no real submissions
export INGEST_CHANNEL=ws
export INGEST_WS_URL=ws://127.0.0.1:8546
export RPC_HTTP_URL=http://127.0.0.1:8545
export STORAGE_DATABASE_URL=postgres://execution_engine:password@127.0.0.1:5432/execution_engine
cargo run -p app
```

For Canary / Live mode see [Execution Modes](#execution-modes) below.

### 4. Docker (optional)

```bash
docker compose -f docker-compose.yml up -d
```

The compose file expects a Base node `geth.ipc` on the host filesystem; see
`docker-compose.yml` for volume mounts and required env vars.

## Architecture

```mermaid
flowchart LR
    subgraph INPUT["Input"]
        FB["Flashblocks WS\npre-confirms"]
        NODE["Base node\nws / ipc / http"]
        FACT["Factories\nPair/PoolCreated"]
    end

    subgraph INGEST["Ingest"]
        I1["streams\n(log / block / tx / flashblocks)"]
        I2["normalizer\nfingerprint + dedup"]
    end

    subgraph DECISION["Decision"]
        M["markets\nPoolBook (V2/V3) + quote math"]
        DISC["discovery\nfactory scan + multicall3"]
        OPP["opportunities\ntwo-leg loop detection"]
        SIM["simulator\nlocal + eth_call"]
        RISK["risk\nnotional / slippage / killswitch"]
        BUILD["builder\nExecutionRequest + calldata"]
    end

    subgraph EXEC["Execution"]
        SIGNER["signer (env / keystore)"]
        NONCE["nonce manager"]
        GAS["EIP-1559 fee estimator"]
        SUBMIT["submit + receipt watcher"]
    end

    subgraph CHAIN["Onchain"]
        ROUTER["BaseExecutionRouter\nV2/V3/Aerodrome + Balancer flashloan"]
    end

    subgraph SHARED["Shared"]
        CFG["config (env)"]
        TYP["types"]
        OBS["observability\ntracing + Prometheus"]
        DB["storage (Postgres)"]
    end

    FB --> I1
    NODE --> I1
    FACT --> DISC
    I1 --> I2 --> M
    DISC --> M
    M --> OPP --> SIM --> RISK --> BUILD
    BUILD --> SUBMIT
    SIGNER --> SUBMIT
    NONCE --> SUBMIT
    GAS --> SUBMIT
    SUBMIT --> ROUTER
    SUBMIT --> DB
    I2 --> DB
    OPP --> DB
    OBS -.metrics.-> SUBMIT
    OBS -.metrics.-> SIM
    OBS -.metrics.-> I2

    classDef input fill:#EAF3FF,stroke:#2B6CB0,color:#102A43,stroke-width:1.5px;
    classDef runtime fill:#EDF7ED,stroke:#2F855A,color:#17331F,stroke-width:1.5px;
    classDef shared fill:#FFF7E8,stroke:#C05621,color:#4A2B0F,stroke-width:1.5px;
    classDef chain fill:#F6EEFF,stroke:#805AD5,color:#2D1F55,stroke-width:1.5px;

    class FB,NODE,FACT input;
    class I1,I2,M,DISC,OPP,SIM,RISK,BUILD,SIGNER,NONCE,GAS,SUBMIT runtime;
    class CFG,TYP,OBS,DB shared;
    class ROUTER chain;
```

## Workspace Layout

```text
crates/
  app/            runtime entrypoint, dependency wiring, main loop
  config/         typed configuration + env loader + validation
  types/          shared domain types: ingest / decision / execution
  registry/       protocol/exchange lookup tables (selectors, signatures)
  rpc/            EngineProvider (HTTP + WS + IPC, retry, multicall3)
  ingest/         WS/IPC pubsub streams + Flashblocks connector + normalizer
  markets/        PoolBook for V2/V3 pools, deterministic quote math
  discovery/      factory event scan + multicall3 hydration into PoolBook
  decision/       opportunity detection, simulation, risk, request builder
  execution/      signer, nonce, gas, eth_call preflight, submit, receipt
  observability/  tracing, Prometheus exporter, healthz, business metrics
  storage/        Postgres pool + migrations + lifecycle persistence
contracts/
  src/BaseExecutionRouter.sol      multi-venue + Balancer flashloan executor
  src/interfaces/                  V2 / V3 / Aerodrome / Balancer ABIs
  test/                            Foundry unit tests + mocks
migration/
  00000000-00-init.sql             engine-applied schema (idempotent)
  ~privileges.sql                  superuser bootstrap (db / role / grants)
```

## Crate Responsibilities

| Crate | Responsibility |
|---|---|
| `app` | Tokio main, loads `AppConfig`, builds `IngestPipeline` / `Storage` / `ExecutionTransport`, drives the event → opportunity → submit loop |
| `config` | `AppConfig` aggregates `Ingest / Rpc / Signer / Decision / Risk / Execution / Storage / Metrics / Discovery`; supports both crate-scoped (`INGEST_*`) and legacy (`BASE_*`) env keys |
| `types` | `Event` / `Log` / `Block` / `Transaction` / `Opportunity` / `SimulationResult` / `RiskDecision` / `ExecutionRequest` / `ExecutionAttempt` / `ExecutionReceipt` / `ExecutionOutcome` |
| `registry` | Static lookup tables: `selector → Protocol/Exchange`, `topic → Protocol/Exchange`, address-type taxonomy |
| `rpc` | `EngineProvider`: HTTP/WS/IPC connections, chain-id verification, request retry, Multicall3 aggregation |
| `ingest` | `IngestPipeline` spawns log/block/transaction streams via `alloy-pubsub`; dedicated `FlashblocksStream` for raw gzipped pre-confirms; emits normalized `Event` |
| `markets` | `PoolBook` keyed by pool address; `V2PoolState` (reserves / fee_bps) and `V3PoolState` (sqrt_price_x96 / liquidity / tick); applies `Swap`/`Sync` logs incrementally |
| `discovery` | `AlloyFactoryReader` scans `PairCreated` / `PoolCreated` from a configured deployment block, batches `getReserves` / `slot0` / `liquidity` via Multicall3 |
| `decision` | `detect_two_leg_loops_from_book`, `simulate_two_leg_opportunity`, `RiskEngine` with rolling notional tracking, `build_execution_request` |
| `execution` | `EngineSigner` (env / keystore), `NonceManager` with chain reconciliation, EIP-1559 `GasCaps`, `AlloySubmitTransport` (preflight `eth_call` → sign → broadcast → poll receipt) |
| `observability` | `ObservabilityRuntime`: JSON tracing, Prometheus exporter, healthz; `metrics` module exposes typed emitters for the 12 business metrics |
| `storage` | `Storage::connect` opens a `PgPool` and applies the consolidated schema migration; persists every observed log / decision run / execution attempt + outcome |

## Configuration

Configuration is loaded from environment variables. Keys grouped by subsystem
(many also accept legacy `BASE_*` aliases for backward compat).

### Required for Live mode

| Env | Description |
|---|---|
| `EXECUTION_MODE` | `shadow` / `canary` / `live` (default `shadow`) |
| `EXECUTION_ROUTER_ADDRESS` | Deployed `BaseExecutionRouter` address |
| `SIGNER_PRIVATE_KEY` | Hex private key (or use `SIGNER_KEYSTORE_PATH` + `SIGNER_KEYSTORE_PASSWORD`) |
| `RPC_HTTP_URL` | HTTP endpoint of the Base node (used for submission + simulation) |
| `INGEST_CHANNEL` | `ws` or `ipc` |
| `INGEST_WS_URL` / `INGEST_IPC_FILE_PATH` | Whichever matches the chosen channel |

### Common knobs

| Env | Default | Notes |
|---|---|---|
| `INGEST_CHAIN_ID` | `8453` | Base mainnet |
| `INGEST_FLASHBLOCKS_WS_URL` | — | Set to `wss://mainnet-preconf.base.org` to subscribe to pre-confirms |
| `INGEST_SUBSCRIBE_{LOGS,BLOCKS,TRANSACTIONS}` | `true` | Per-stream toggle |
| `RISK_MIN_NET_PROFIT_WEI` | `0` | Floor on simulated net profit |
| `RISK_MIN_PROFIT_BPS` | `0` | Floor in bps of input notional |
| `RISK_MAX_TRADE_NOTIONAL_WEI` | unlimited | Per-trade cap |
| `RISK_MAX_PER_BLOCK_NOTIONAL_WEI` | unlimited | Rolling per-block cap |
| `RISK_MAX_DAILY_NOTIONAL_WEI` | unlimited | Rolling 24h cap |
| `RISK_MAX_SLIPPAGE_BPS` | `100` | Hard ceiling on per-step slippage |
| `RISK_KILL_SWITCH_PATH` | — | If file exists at this path, no submissions |
| `EXECUTION_GAS_LIMIT_MULTIPLIER_BPS` | `12000` | 1.2× safety multiplier on estimated gas |
| `EXECUTION_MAX_FEE_PER_GAS_WEI` | unlimited | Hard cap on EIP-1559 max fee |
| `EXECUTION_MAX_PRIORITY_FEE_PER_GAS_WEI` | unlimited | Hard cap on priority fee |
| `EXECUTION_PLAN_DEADLINE_SECS` | `30` | Router-side deadline written into calldata |
| `EXECUTION_ETHCALL_PREFLIGHT_REQUIRED` | `true` | Strict (`true`) vs advisory (`false`) eth_call gate |
| `EXECUTION_ETHCALL_TIMEOUT_SECS` | `5` | Slow-path simulation timeout |
| `EXECUTION_SUBMIT_TIMEOUT_SECS` | `30` | Submit-side wall-clock timeout |
| `EXECUTION_RECEIPT_TIMEOUT_SECS` | `60` | Receipt-poll deadline (after which → `Dropped`) |
| `EXECUTION_CANARY_MAX_TRADE_WEI` / `..._MAX_DAILY_WEI` | — | Required when `EXECUTION_MODE=canary` |
| `STORAGE_DATABASE_URL` | — | If empty, persistence is disabled |
| `STORAGE_AUTO_MIGRATE` | `true` | Apply `00000000-00-init.sql` on boot |
| `STORAGE_MAX_CONNECTIONS` | `10` | PgPool size |
| `METRICS_ENABLED` | `true` | Master switch for Prometheus + business metrics |
| `METRICS_PROMETHEUS_BIND_ADDR` | — | e.g. `0.0.0.0:9100` to expose `/metrics` |
| `METRICS_HEALTHZ_BIND_ADDR` | `0.0.0.0:9000` | `/healthz` returns `200 ok` |
| `LOG_LEVEL` | `info` | `tracing-subscriber` filter spec |
| `DISCOVERY_ENABLED` | `false` | Run factory scan + Multicall3 hydration on boot |
| `DISCOVERY_FACTORIES_PATH` | — | JSON file describing factories (see `config/discovery-factories.example.json`) |

## Execution Modes

| Mode | Submits txs? | Required env | Use case |
|---|---|---|---|
| `shadow` | No | minimal — ingest + storage only | Passive observation, dataset capture, dry-run |
| `canary` | Yes, capped | `EXECUTION_ROUTER_ADDRESS`, signer, `EXECUTION_CANARY_MAX_TRADE_WEI`, `EXECUTION_CANARY_MAX_DAILY_WEI` | Live tiny-size trading to validate the full path before opening the throttle |
| `live` | Yes, full | router, signer, RPC HTTP, all risk caps configured | Production trading |

In `shadow` mode the engine instantiates `UnavailableTransport` which logs +
records every would-be submission but never broadcasts. Switching to
`canary` / `live` requires a working HTTP RPC for `eth_sendRawTransaction`,
`eth_call`, and `eth_getTransactionReceipt`.

## Observability

Tracing and metrics are owned by the `observability` crate.

- **Tracing:** structured JSON to stdout, filter via `LOG_LEVEL` env or
  `RUST_LOG`. All spans carry `request_id` / `tx_hash` / `opportunity_id`
  where applicable.
- **Healthz:** `GET /healthz` on `METRICS_HEALTHZ_BIND_ADDR` (default
  `:9000`) returns `200 ok`.
- **Prometheus:** `GET /metrics` on `METRICS_PROMETHEUS_BIND_ADDR` (e.g.
  `:9100`) exposes process metrics + the business metrics below.

### Business metrics (12 total)

| Metric | Type | Labels |
|---|---|---|
| `ingest_events_total` | counter | `stream`, `channel`, `decode_status` |
| `ingest_decision_lag_seconds` | histogram | `stream` |
| `decision_opportunities_total` | counter | `outcome` |
| `simulator_local_seconds` | histogram | — |
| `simulator_ethcall_seconds` | histogram | — |
| `simulator_outcomes_total` | counter | `kind`, `outcome` |
| `execution_submit_seconds` | histogram | — |
| `execution_receipt_seconds` | histogram | — |
| `execution_outcomes_total` | counter | `status` |
| `execution_realized_surplus_wei` | histogram | — |
| `execution_realized_surplus_total_wei` | gauge | — |
| `execution_fee_paid_total_wei` | counter | — |

All metrics are described via `# HELP` lines on registration; introspect
them at runtime by curling the `/metrics` endpoint.

## Database

Three tables persist the entire pipeline (definitions in
[`migration/00000000-00-init.sql`](migration/00000000-00-init.sql)):

| Table | Row per | Notes |
|---|---|---|
| `observed_logs` | every raw log surfaced by ingest | de-duplicated by `fingerprint`; full payload in `log_json` |
| `decision_runs` | every opportunity that survived detection + simulation | references `observed_logs(id)`; opportunity / simulation / risk JSON for replay |
| `execution_attempts` | every attempted submission | full lifecycle: `built → submitted → included | reverted | dropped | replaced`; receipt + outcome JSON; gas + fee + realized surplus broken out |

Roles defined by `migration/~privileges.sql`:

- **`execution_engine`** — runtime read/write, owns the `arbitrage` schema
- **`execution_engine_ro`** — read-only, intended for Grafana / analytics
  / on-call exploration; gets `SELECT` on existing + future tables via
  `ALTER DEFAULT PRIVILEGES`

Bootstrap once with `psql -f migration/~privileges.sql` as superuser, then
the engine handles schema migrations on every subsequent boot.

## Smart Contracts

The `BaseExecutionRouter` (Solidity ^0.8.24) is the single onchain entry
point for execution. Highlights:

- `executePlan` — self-funded multi-leg execution
- `executePlanWithFlashLoan` — Balancer V2 flashloan callback that runs the
  same route validator under the hood (Balancer V2 charges 0 fee on Base;
  the contract still asserts `feeAmounts == 0` defensively)
- Per-step `minAmountOut`, global `minRepayAmount + minSurplus` invariant,
  unified `deadline`, `Pausable + Ownable + ReentrancyGuard`
- Venue adapters for `UniswapV2`, `UniswapV3Single`, `AerodromeV2`
  (Solidly fork) — adding a venue is enum + adapter, the route validator
  stays unchanged
- The Balancer callback is gated on a transient `_loanContext` slot so a
  random caller cannot trigger `receiveFlashLoan`

Foundry tests cover per-venue swaps, flashloan round-trip, slippage
reverts, and access control. To extend coverage to real Base mainnet state,
run with a fork URL:

```bash
forge test --root contracts --fork-url $BASE_RPC_URL
```

## License & Disclaimer

Licensed per the workspace `Cargo.toml` (`license.workspace = true`); see
the workspace manifest for the canonical SPDX identifier.

This software is **experimental**. Running it in `canary` or `live` mode
will sign and broadcast transactions that move real funds. You are solely
responsible for the keys you load, the router you deploy, the risk caps
you configure, and any losses incurred. The authors provide no warranty,
express or implied. Audit the contract, simulate aggressively, and start
in `shadow` mode.
