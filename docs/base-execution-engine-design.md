# Base Execution Engine Design

## Overview

Base Execution Engine is a system for pre-confirmation transaction ingestion,
multi-protocol decoding, route discovery, simulation, policy evaluation, and
execution on Base.

The system is built around two core infrastructure inputs:

- Base Flashblocks for pre-confirmation transaction and log visibility
- A self-hosted Base node for canonical chain state, low-latency reads, and
  simulation

The design supports three runtime modes:

- `research`
- `shadow`
- `live`

## Goals

- Ingest pre-confirmation transaction data from Base with low latency
- Normalize protocol-specific interactions into a canonical intent model
- Discover executable routes across heterogeneous liquidity venues
- Simulate candidate routes against current Base state before execution
- Enforce policy and safety constraints before any live action
- Preserve deterministic runtime behavior, observability, and recoverability

## Non-Goals

- A monolithic strategy-specific bot implementation
- A block-only event scanner
- A contract system that hardcodes one execution pattern
- A persistence model based on ad hoc local files

## Runtime Modes

### Research

- Pre-confirmation ingress is enabled
- Decoding, route discovery, and simulation are enabled
- No broadcast path is enabled

### Shadow

- Full decision pipeline is enabled
- Decisions are persisted and observable
- Broadcast is disabled

### Live

- Full decision pipeline is enabled
- Broadcast is enabled behind explicit policy gates
- Contract execution and transaction submission are allowed

## External Dependencies

### Flashblocks

Flashblocks is the primary pre-confirmation ingress layer.

Responsibilities:

- Deliver pre-confirmation transaction updates
- Deliver pre-confirmation log updates
- Provide sub-block visibility for decisioning

### Self-Hosted Base Node

The local Base node is the canonical state backend.

Responsibilities:

- Expose IPC for low-latency reads
- Expose canonical block and state data
- Back simulation and verification
- Provide execution-time state and gas context

## High-Level Architecture

```text
Flashblocks
    |
    v
Ingress Layer
    |
    v
Decode Layer
    |
    v
Enrichment + Graph Planner
    |
    v
Simulation Layer
    |
    v
Policy Layer
    |
    v
Execution Layer
    |
    v
State + Observability
```

## Workspace Layout

```text
crates/
  app/
  common/
  config/
  decode/
  graph/
  ingress/
  simulation/
  strategy/
contracts/
  BaseExecutionRouter.sol
config/
  base-liquidity-seeds.example.json
docs/
  base-execution-engine-design.md
```

## Module Responsibilities

### `app`

`app` owns runtime composition and supervision.

Responsibilities:

- Load validated configuration
- Initialize logging
- Connect ingress providers
- Wire decode, planning, simulation, and execution components
- Supervise the runtime event loop

### `common`

`common` owns shared data structures and Base-specific constants.

Responsibilities:

- Common enums and models
- Address book for Base venues
- Canonical route and simulation types
- Shared utility classification helpers

Core types:

- `Mode`
- `EventSource`
- `VenueKind`
- `ActionKind`
- `PoolKey`
- `PoolEdge`
- `RouteStep`
- `RoutePlan`
- `SimulationResult`
- `ExecutionDecision`
- `RawTransactionEnvelope`
- `ObservedIntent`
- `IngressEvent`

### `config`

`config` owns runtime configuration.

Responsibilities:

- Read environment variables
- Validate runtime mode and constraints
- Provide typed config to all runtime services

Current configuration areas:

- runtime mode
- Base chain id
- local IPC path
- Flashblocks endpoint
- planner limits
- log level

### `ingress`

`ingress` owns connectivity to external event and state sources.

Responsibilities:

- Connect to Flashblocks WebSocket
- Subscribe to pre-confirmation transaction feeds
- Subscribe to pre-confirmation log feeds
- Connect to the local Base IPC provider
- Normalize ingress payloads into `IngressEvent`
- Handle connection lifecycle and heartbeat events

Subcomponents:

- `FlashblocksClient`
- `ChainClient`

### `decode`

`decode` transforms raw transaction envelopes into canonical observed intents.

Responsibilities:

- Extract selectors from calldata
- Decode common Uniswap V2, V3, V4, Aerodrome, and Virtuals patterns
- Extract V4 pool key metadata from Universal Router payloads
- Classify action and venue

Output:

- `ObservedIntent`

Canonical intent shape:

```rust
pub struct ObservedIntent {
    pub source: EventSource,
    pub tx_hash: H256,
    pub actor: Address,
    pub venue: VenueKind,
    pub action: ActionKind,
    pub token_in: Option<Address>,
    pub token_out: Option<Address>,
    pub amount_in: Option<U256>,
    pub pool_key: Option<PoolKey>,
    pub raw_selector: Option<String>,
    pub metadata: Value,
}
```

### `graph`

`graph` owns liquidity representation and route discovery.

Responsibilities:

- Represent Base liquidity venues as graph edges
- Maintain adjacency between tokens
- Search bounded candidate routes
- Rank routes by projected output and gas

Graph model:

- vertex: token
- edge: executable liquidity venue

Edge attributes:

- venue kind
- router
- quoter
- token in
- token out
- fee basis points
- path
- pool key
- stable flag
- estimated gas
- liquidity score

Planner behavior:

- bounded hop search
- cycle avoidance
- candidate ranking
- candidate truncation by configured limits

Static venue metadata can be seeded from
`config/base-liquidity-seeds.example.json`
before dynamic graph maintenance is added.

### `strategy`

`strategy` owns venue-specific adapters.

Responsibilities:

- Encode quote calls
- Decode quote outputs
- Encode swap calls
- Keep venue-specific ABI concerns isolated from the runtime

Current adapter shape:

```rust
pub trait ExecutionAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn venue(&self) -> VenueKind;
    fn encode_quote(
        &self,
        amount_in: U256,
        token_in: Address,
        token_out: Address,
    ) -> Result<ContractCall>;
    fn decode_quote(&self, output: Bytes) -> Result<U256>;
    fn encode_swap(
        &self,
        amount_in: U256,
        token_in: Address,
        token_out: Address,
        recipient: Address,
        amount_out_min: U256,
        deadline: U256,
    ) -> Result<ContractCall>;
}
```

Current adapter coverage:

- Uniswap V3
- Uniswap V2-style routers
- Aerodrome V2
- Uniswap V4 via Universal Router

### `simulation`

`simulation` owns route-level evaluation against current chain state.

Responsibilities:

- Evaluate candidate route plans
- Execute quote calls in sequence
- Aggregate projected output and gas
- Return normalized simulation results

Current simulation scope:

- quote-driven route simulation over the local Base node

Planned extension:

- full `revm` route simulation
- atomic multi-step execution simulation
- revert classification
- confidence scoring from stateful execution

## Current Runtime Flow

1. `app` starts with validated config
2. `ingress` connects to Flashblocks and the local Base node
3. Flashblocks emits transaction or log payloads
4. `ingress` converts payloads into `IngressEvent`
5. `decode` converts transaction envelopes into `ObservedIntent`
6. `graph` builds or queries candidate routes for a target token pair
7. `simulation` evaluates candidate `RoutePlan` values
8. The runtime produces a decision based on mode and policy
9. In live mode, execution is delegated to the execution layer and contract path

## Data Models

### Ingress Event

Ingress output is normalized into one of:

- `Transaction`
- `Log`
- `Heartbeat`

### Raw Transaction Envelope

`RawTransactionEnvelope` preserves raw transaction context before decode.

Fields:

- source
- transaction hash
- sender
- receiver
- calldata
- value
- optional block number
- raw payload

### Pool Edge

`PoolEdge` is the planner’s executable venue abstraction.

Fields:

- human-readable name
- venue kind
- router
- optional quoter
- token in
- token out
- fee basis points
- path
- pool key
- stable flag
- estimated gas
- liquidity score

### Route Plan

`RoutePlan` is the planner output.

Fields:

- source token
- target token
- amount in
- ordered route steps
- expected amount out
- estimated gas

### Simulation Result

`SimulationResult` is the simulation layer output.

Fields:

- status
- expected amount out
- gas used
- reason
- confidence basis points

## Contract Design

The execution contract is `BaseExecutionRouter`.

Current goals:

- Support guarded route execution
- Keep execution authority explicit
- Support pausing
- Validate minimum output
- Emit execution events

### Contract Model

```solidity
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
```

### Contract Responsibilities

- Authenticate executors
- Execute approved plans
- Enforce pause state
- Enforce minimum output
- Return funds to the caller after successful execution

### Contract Constraints

- The current contract only supports a constrained venue set
- Additional venue adapters must be added explicitly
- Rust-side planning and Solidity-side execution schemas must remain aligned

## Configuration Model

Current environment variables:

- `ENGINE_MODE`
- `BASE_IPC_PATH`
- `FLASHBLOCKS_WS_URL`
- `BASE_CHAIN_ID`
- `LOG_LEVEL`
- `MAX_ROUTE_HOPS`
- `MAX_CANDIDATE_ROUTES`
- `FLASHBLOCKS_SUBSCRIBE_TRANSACTIONS`
- `FLASHBLOCKS_SUBSCRIBE_PENDING_LOGS`

## Operational Model

### Logging

Structured logs are emitted from runtime components.

Important log points:

- startup health
- Flashblocks connection state
- decode success and failure
- route planning selection
- simulation output
- runtime shutdown

### Failure Handling

The current design expects:

- transient websocket failures
- malformed or partial ingress payloads
- undecodable protocol payloads
- quote failures
- route search miss cases

Current handling principles:

- normalize failures into typed results where possible
- preserve raw payloads for debugging
- avoid panics in the transaction path
- allow the runtime to continue on individual event failure

## Deployment Model

### Single-Host Deployment

Recommended base topology:

- `engine` process
- local Base node
- contract deployment environment

Optional observability services:

- Prometheus
- Grafana
- Alertmanager

### Process Boundaries

The first production shape remains a single runtime binary with internal module
separation rather than multiple networked services.

This keeps:

- operational complexity lower
- latency lower
- state sharing simpler

## State and Persistence Direction

The runtime model assumes durable state will be added behind explicit state
transitions.

Primary persistence targets:

- observed intents
- route decisions
- simulation outputs
- submitted execution records
- position lifecycle state

Preferred backing store:

- SQLite for single-host deployment

## Security and Safety

### Runtime Safety

- Live execution is mode-gated
- Execution should only occur after decode, planning, and simulation
- Planner limits are configuration-driven
- Contract execution authority is explicit

### Contract Safety

- owner-controlled executor registry
- pause support
- minimum output enforcement
- explicit venue kind validation

## Current Limitations

- Policy evaluation is not yet separated into its own crate
- Full stateful `revm` route execution is not yet implemented in the new runtime
- Durable storage is not yet integrated
- Contract venue support is intentionally narrow
- The planner currently uses bounded search over loaded graph edges rather than a
  continuously maintained market graph

## Design Principles

- Flashblocks is the primary trigger path
- The local Base node is the canonical state backend
- Runtime components communicate through typed models
- Venue-specific logic remains isolated behind adapters
- Execution must remain guarded, observable, and mode-aware
