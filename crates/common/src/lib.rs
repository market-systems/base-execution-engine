use ethers::types::{Address, Bytes, H256, I256, U256};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

pub const BASE_CHAIN_ID: u64 = 8453;

pub static WETH_BASE: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x4200000000000000000000000000000000000006").unwrap());
pub static USDC_BASE: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap());

pub static BASESWAP_ROUTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x2948acbbc8795267e62a1220683a48e718b52585").unwrap());
pub static ALIENBASE_ROUTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x8c1A3cF8f83074169FE5D7aD50B978e1cd6b37c7").unwrap());
pub static UNIV3_ROUTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x2626664c2603336E57B271c5C0b26F421741e481").unwrap());
pub static UNIV3_QUOTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x3d4e44Eb1374240CE5F1B871ab261CD16335B76a").unwrap());
pub static AERODROME_ROUTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43").unwrap());
pub static AERODROME_FACTORY: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x420DD381b31aEf6683db6B902084cB0FFECe40Da").unwrap());
pub static AERO_V3_ROUTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0xBE6D8f0d05cC4be24d5167a3eF062215bE6D18a5").unwrap());
pub static AERO_V3_QUOTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x254cF9E1E6e233aa1Ac962cb9B05b2cfeAaE15b0").unwrap());
pub static PANCAKESWAP_V3_ROUTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x1b81D678ffb9C0263b24A97847620C99d213eB14").unwrap());
pub static PANCAKESWAP_V3_QUOTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0xB048Bbc1Ee6b733FFfCFb9e9CeF7375518e25997").unwrap());
pub static UNIV4_QUOTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x0d5e0f971ed27fbff6c2837bf31316121532048d").unwrap());
pub static UNIVERSAL_ROUTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x743f2f29cdd66242fb27d292ab2cc92f45674635").unwrap());
pub static CLANKER_HOOK_STATIC: Lazy<Address> =
    Lazy::new(|| Address::from_str("0xb429d62f8f3bFFb98CdB9569533eA23bF0Ba28CC").unwrap());
pub static CLANKER_HOOK_DYNAMIC: Lazy<Address> =
    Lazy::new(|| Address::from_str("0xd60D6B218116cFd801E28F78d011a203D2b068Cc").unwrap());
pub static VIRTUALS_ROUTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0x0b3e328455c4059EEb9e3f84b5543F74E24e7E1b").unwrap());
pub static VIRTUALS_FACTORY_ROUTER: Lazy<Address> =
    Lazy::new(|| Address::from_str("0xc479b79e53c1065e5e56a6da78e9d634b4ae1e5d").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Research,
    Shadow,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Flashblocks,
    LocalChain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenueKind {
    Unknown,
    UniswapV2,
    UniswapV3,
    UniswapV4,
    AerodromeV2,
    Virtuals,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Unknown,
    BuyLike,
    SellLike,
    Swap,
    AddLiquidity,
    ProtocolInteraction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolKey {
    pub currency0: Address,
    pub currency1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub hooks: Address,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractCall {
    pub target: Address,
    pub calldata: Bytes,
    pub value: U256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VenueCapabilities {
    pub discoverable: bool,
    pub plannable: bool,
    pub simulatable: bool,
    pub executable: bool,
}

impl VenueCapabilities {
    pub const fn new(
        discoverable: bool,
        plannable: bool,
        simulatable: bool,
        executable: bool,
    ) -> Self {
        Self {
            discoverable,
            plannable,
            simulatable,
            executable,
        }
    }
}

impl Default for VenueCapabilities {
    fn default() -> Self {
        Self::new(true, true, true, false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolEdge {
    pub name: String,
    pub venue: VenueKind,
    #[serde(default)]
    pub capabilities: VenueCapabilities,
    pub router: Address,
    pub quoter: Option<Address>,
    pub token_in: Address,
    pub token_out: Address,
    pub fee_bps: u32,
    pub liquidity_score: u128,
    pub path: Vec<Address>,
    pub stable: bool,
    pub pool_key: Option<PoolKey>,
    pub estimated_gas: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteStep {
    pub venue: VenueKind,
    pub name: String,
    #[serde(default)]
    pub capabilities: VenueCapabilities,
    pub router: Address,
    pub quoter: Option<Address>,
    pub token_in: Address,
    pub token_out: Address,
    pub fee_bps: u32,
    pub path: Vec<Address>,
    pub stable: bool,
    pub pool_key: Option<PoolKey>,
    pub estimated_gas: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutePlan {
    pub source_token: Address,
    pub target_token: Address,
    pub amount_in: U256,
    pub steps: Vec<RouteStep>,
    pub expected_amount_out: U256,
    pub estimated_gas: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulationStatus {
    Success,
    Reverted,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub status: SimulationStatus,
    pub expected_amount_out: U256,
    pub gas_used: u64,
    pub estimated_gas_cost: U256,
    pub gross_surplus: I256,
    pub net_surplus: I256,
    pub post_trigger_checked: bool,
    pub reason: String,
    pub confidence_bps: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Approved,
    Denied,
    Deferred,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionDecision {
    pub status: DecisionStatus,
    pub reason: String,
    pub plan: Option<RoutePlan>,
    pub simulation: Option<SimulationResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub observed_tx_hash: H256,
    pub route_plan: RoutePlan,
    pub simulation: SimulationResult,
    pub router: Address,
    pub plan: RouterExecutionPlan,
    pub calls: Vec<ContractCall>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionVenueKind {
    Unsupported,
    UniswapV3Single,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlanStep {
    pub venue_kind: ExecutionVenueKind,
    pub target: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub fee_bps: u32,
    pub extra_data: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterExecutionPlan {
    pub settlement_token: Address,
    pub funding_amount: U256,
    pub min_repay_amount: U256,
    pub min_surplus: U256,
    pub steps: Vec<ExecutionPlanStep>,
    pub risk_hash: H256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStatus {
    Suppressed,
    Submitted,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchResult {
    pub status: DispatchStatus,
    pub tx_hash: Option<H256>,
    pub submitted_from: Option<Address>,
    pub submitted_nonce: Option<u64>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationStatus {
    Approved,
    Rejected,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationRejectionReason {
    UnsupportedAction,
    MissingTokenIn,
    MissingTokenOut,
    MissingAmountIn,
    NoCandidateRoutes,
    SimulationFailed,
    PolicyDenied,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PipelineTimings {
    pub planning_micros: u64,
    pub simulation_micros: u64,
    pub policy_micros: u64,
    pub total_micros: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityCandidate {
    pub intent: ObservedIntent,
    pub settlement_token: Address,
    pub target_token: Address,
    pub amount_in: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedOpportunity {
    pub candidate: OpportunityCandidate,
    pub routes: Vec<RoutePlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEvaluation {
    pub tx_hash: H256,
    pub candidate: Option<OpportunityCandidate>,
    pub routes_considered: usize,
    pub decision: ExecutionDecision,
    pub status: EvaluationStatus,
    pub rejection_reason: Option<EvaluationRejectionReason>,
    pub timings: PipelineTimings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentStatus {
    Qualified,
    Rejected,
    FilteredOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    PolicyChecked,
    ReadyToSubmit,
    BroadcastSuppressed,
    BroadcastSubmitted,
    BroadcastFailed,
    Included,
    OnchainReverted,
    Replaced,
    Dropped,
    TimedOut,
    Rejected,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionIntent {
    pub id: String,
    pub observed_tx_hash: H256,
    pub actor: Option<Address>,
    pub source: Option<EventSource>,
    pub trigger: Option<ObservedIntent>,
    pub status: IntentStatus,
    pub created_at_micros: u64,
    pub updated_at_micros: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionAttempt {
    pub id: String,
    pub intent_id: String,
    pub routes_considered: usize,
    pub candidate: Option<OpportunityCandidate>,
    pub decision: ExecutionDecision,
    pub request: Option<ExecutionRequest>,
    pub dispatch: Option<DispatchResult>,
    #[serde(alias = "shadow_status")]
    pub evaluation_status: EvaluationStatus,
    pub rejection_reason: Option<EvaluationRejectionReason>,
    pub timings: PipelineTimings,
    pub status: AttemptStatus,
    pub fail_reason: Option<String>,
    pub receipt_block_number: Option<u64>,
    pub receipt_gas_used: Option<u64>,
    pub created_at_micros: u64,
    pub updated_at_micros: u64,
    pub version: u64,
}

impl ExecutionAttempt {
    pub fn bump_version(mut self) -> Self {
        self.version = self.version.saturating_add(1);
        self.updated_at_micros = self.updated_at_micros.saturating_add(1);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawTransactionEnvelope {
    pub source: EventSource,
    pub tx_hash: H256,
    pub from: Address,
    pub to: Option<Address>,
    pub input: Bytes,
    pub value: U256,
    pub block_number: Option<u64>,
    pub raw_payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedIntent {
    pub source: EventSource,
    pub tx_hash: H256,
    pub actor: Address,
    pub router: Address,
    pub venue: VenueKind,
    pub action: ActionKind,
    pub token_in: Option<Address>,
    pub token_out: Option<Address>,
    pub amount_in: Option<U256>,
    pub pool_key: Option<PoolKey>,
    pub input: Bytes,
    pub value: U256,
    pub raw_selector: Option<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IngressEvent {
    Transaction(RawTransactionEnvelope),
    Log {
        source: EventSource,
        payload: Value,
    },
    Heartbeat {
        source: EventSource,
    },
}

impl RoutePlan {
    pub fn token_path(&self) -> Vec<Address> {
        let mut path = Vec::with_capacity(self.steps.len() + 1);
        path.push(self.source_token);
        for step in &self.steps {
            path.push(step.token_out);
        }
        path
    }

    pub fn is_simulatable(&self) -> bool {
        self.steps.iter().all(|step| step.capabilities.simulatable)
    }

    pub fn is_executable(&self) -> bool {
        self.steps.iter().all(|step| step.capabilities.executable)
    }

    pub fn is_cycle(&self) -> bool {
        self.source_token == self.target_token
            && self
                .steps
                .last()
                .map(|step| step.token_out == self.source_token)
                .unwrap_or(false)
    }
}

impl From<&PoolEdge> for RouteStep {
    fn from(edge: &PoolEdge) -> Self {
        Self {
            venue: edge.venue,
            name: edge.name.clone(),
            capabilities: edge.capabilities,
            router: edge.router,
            quoter: edge.quoter,
            token_in: edge.token_in,
            token_out: edge.token_out,
            fee_bps: edge.fee_bps,
            path: edge.path.clone(),
            stable: edge.stable,
            pool_key: edge.pool_key.clone(),
            estimated_gas: edge.estimated_gas,
        }
    }
}

pub fn default_capabilities_for_venue(venue: VenueKind) -> VenueCapabilities {
    match venue {
        VenueKind::UniswapV3 => VenueCapabilities::new(true, true, true, true),
        VenueKind::UniswapV2 => VenueCapabilities::new(true, true, true, false),
        VenueKind::AerodromeV2 => VenueCapabilities::new(true, true, true, false),
        VenueKind::UniswapV4 => VenueCapabilities::new(true, true, true, false),
        VenueKind::Virtuals => VenueCapabilities::new(true, true, false, false),
        VenueKind::Unknown => VenueCapabilities::new(false, false, false, false),
    }
}

pub fn router_name(address: &Address) -> &'static str {
    match *address {
        address if address == *BASESWAP_ROUTER => "BaseSwap",
        address if address == *ALIENBASE_ROUTER => "AlienBase",
        address if address == *UNIV3_ROUTER => "UniswapV3",
        address if address == *AERODROME_ROUTER => "Aerodrome",
        address if address == *AERO_V3_ROUTER => "AerodromeSlipstream",
        address if address == *PANCAKESWAP_V3_ROUTER => "PancakeSwapV3",
        address if address == *UNIVERSAL_ROUTER => "UniversalRouter",
        address if address == *VIRTUALS_ROUTER => "VirtualsRouter",
        address if address == *VIRTUALS_FACTORY_ROUTER => "VirtualsFactory",
        _ => "Unknown",
    }
}

pub fn classify_venue(address: &Address) -> VenueKind {
    match *address {
        address if address == *UNIV3_ROUTER => VenueKind::UniswapV3,
        address if address == *PANCAKESWAP_V3_ROUTER => VenueKind::UniswapV3,
        address if address == *AERO_V3_ROUTER => VenueKind::UniswapV3,
        address if address == *AERODROME_ROUTER => VenueKind::AerodromeV2,
        address if address == *UNIVERSAL_ROUTER => VenueKind::UniswapV4,
        address if address == *VIRTUALS_ROUTER => VenueKind::Virtuals,
        address if address == *VIRTUALS_FACTORY_ROUTER => VenueKind::Virtuals,
        address
            if address == *BASESWAP_ROUTER || address == *ALIENBASE_ROUTER =>
        {
            VenueKind::UniswapV2
        }
        _ => VenueKind::Unknown,
    }
}
