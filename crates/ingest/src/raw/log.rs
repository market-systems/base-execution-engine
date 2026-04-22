use crate::raw::{as_object, bool_field, string_field, u64_field};
use alloy_primitives::{
    aliases::{I24, U112},
    hex, Address as AlloyAddress, B256, I256, U160, U256,
};
use alloy_sol_types::{sol, SolEvent};
use registry::resolve_log;
use serde_json::Value;
use types::ingest::{
    BlockContext, DecodedLogEvent, DecodedLogKind, Exchange, Log, Metadata, Protocol,
};
use types::{Address, BlockHash, Topic, TxHash};

sol! {
    event Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to);
    event Sync(uint112 reserve0, uint112 reserve1);
    event Mint(address indexed sender, uint256 amount0, uint256 amount1);
    event Burn(address indexed sender, uint256 amount0, uint256 amount1, address indexed to);
}

mod v3_events {
    use alloy_sol_types::sol;

    sol! {
    event Swap(
        address indexed sender,
        address indexed recipient,
        int256 amount0,
        int256 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick
    );

    event Mint(
        address sender,
        address indexed owner,
        int24 indexed tickLower,
        int24 indexed tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1
    );

    event Burn(
        address indexed owner,
        int24 indexed tickLower,
        int24 indexed tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1
    );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLogMessage {
    pub address: Option<Address>,
    pub topics: Vec<Topic>,
    pub data: Option<String>,
    pub tx_hash: Option<TxHash>,
    pub block_hash: Option<BlockHash>,
    pub block_number: Option<u64>,
    pub event_signature: Option<String>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
    pub log_index: Option<u64>,
    pub removed: Option<bool>,
}

impl RawLogMessage {
    pub fn from_value(value: &Value) -> Option<Self> {
        let object = as_object(value)?;
        let topics = object
            .get("topics")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Some(Self {
            address: string_field(object, "address"),
            topics,
            data: string_field(object, "data"),
            tx_hash: string_field(object, "transactionHash"),
            block_hash: string_field(object, "blockHash"),
            block_number: u64_field(object, "blockNumber"),
            event_signature: string_field(object, "eventSignature"),
            protocol: None,
            exchange: None,
            log_index: u64_field(object, "logIndex"),
            removed: bool_field(object, "removed"),
        })
    }

    pub fn block_context(&self) -> BlockContext {
        match self.block_number {
            Some(number) => BlockContext::Block {
                number,
                hash: self.block_hash.clone(),
            },
            None => BlockContext::Pending,
        }
    }

    pub fn decode(&self) -> DecodedLogFields {
        let event_signature = self.event_signature();
        let resolution = resolve_log(event_signature.as_deref(), self.address.as_deref());
        let decoded_event = self.decode_structured_event(
            event_signature.as_deref(),
            resolution.protocol,
            resolution.exchange,
        );

        DecodedLogFields {
            protocol: resolution.protocol.or(self.protocol),
            exchange: resolution.exchange.or(self.exchange),
            event_signature,
            decoded_event,
        }
    }

    pub fn event_signature(&self) -> Option<String> {
        self.event_signature
            .clone()
            .or_else(|| self.topics.first().cloned())
    }

    pub fn to_log(self, metadata: Metadata) -> Log {
        let decoded = self.decode();

        Log {
            metadata,
            address: self.address,
            topics: self.topics,
            data: self.data,
            event_signature: decoded.event_signature,
            decoded_event: decoded.decoded_event,
            protocol: decoded.protocol,
            exchange: decoded.exchange,
            log_index: self.log_index,
            removed: self.removed,
        }
    }

    fn decode_structured_event(
        &self,
        event_signature: Option<&str>,
        protocol: Option<Protocol>,
        exchange: Option<Exchange>,
    ) -> Option<DecodedLogEvent> {
        let event_signature = event_signature?;
        let topics = decode_topics(&self.topics)?;
        let data = decode_data(self.data.as_deref().unwrap_or("0x"))?;
        let pool = self.address.clone();

        match event_signature {
            "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822" => {
                let event = Swap::decode_raw_log(topics.clone(), &data, true).ok()?;
                Some(DecodedLogEvent {
                    kind: DecodedLogKind::V2Swap,
                    pool,
                    sender: Some(format_address(event.sender)),
                    recipient: Some(format_address(event.to)),
                    owner: None,
                    reserve0: None,
                    reserve1: None,
                    amount0_in: Some(u256_to_u128(event.amount0In)?),
                    amount1_in: Some(u256_to_u128(event.amount1In)?),
                    amount0_out: Some(u256_to_u128(event.amount0Out)?),
                    amount1_out: Some(u256_to_u128(event.amount1Out)?),
                    amount0: None,
                    amount1: None,
                    liquidity: None,
                    sqrt_price_x96: None,
                    tick: None,
                    tick_lower: None,
                    tick_upper: None,
                    protocol,
                    exchange,
                })
            }
            "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1" => {
                let event = Sync::decode_raw_log(topics.clone(), &data, true).ok()?;
                Some(DecodedLogEvent {
                    kind: DecodedLogKind::V2Sync,
                    pool,
                    sender: None,
                    recipient: None,
                    owner: None,
                    reserve0: Some(u112_to_u128(event.reserve0)),
                    reserve1: Some(u112_to_u128(event.reserve1)),
                    amount0_in: None,
                    amount1_in: None,
                    amount0_out: None,
                    amount1_out: None,
                    amount0: None,
                    amount1: None,
                    liquidity: None,
                    sqrt_price_x96: None,
                    tick: None,
                    tick_lower: None,
                    tick_upper: None,
                    protocol,
                    exchange,
                })
            }
            "0x4c209b5fc8ad50758f13e2e108bdbdff36c9b4a5d2f30f6a7c2a25d13f0f5e6d" => {
                let event = Mint::decode_raw_log(topics.clone(), &data, true).ok()?;
                Some(DecodedLogEvent {
                    kind: DecodedLogKind::V2Mint,
                    pool,
                    sender: Some(format_address(event.sender)),
                    recipient: None,
                    owner: None,
                    reserve0: None,
                    reserve1: None,
                    amount0_in: None,
                    amount1_in: None,
                    amount0_out: None,
                    amount1_out: None,
                    amount0: Some(u256_to_string(event.amount0)),
                    amount1: Some(u256_to_string(event.amount1)),
                    liquidity: None,
                    sqrt_price_x96: None,
                    tick: None,
                    tick_lower: None,
                    tick_upper: None,
                    protocol,
                    exchange,
                })
            }
            "0xdccd412f0b1252819a70d2cd3248e4723d18f7e41ee2b5f7f8ef4e3d5be2f4d8" => {
                let event = Burn::decode_raw_log(topics.clone(), &data, true).ok()?;
                Some(DecodedLogEvent {
                    kind: DecodedLogKind::V2Burn,
                    pool,
                    sender: Some(format_address(event.sender)),
                    recipient: Some(format_address(event.to)),
                    owner: None,
                    reserve0: None,
                    reserve1: None,
                    amount0_in: None,
                    amount1_in: None,
                    amount0_out: None,
                    amount1_out: None,
                    amount0: Some(u256_to_string(event.amount0)),
                    amount1: Some(u256_to_string(event.amount1)),
                    liquidity: None,
                    sqrt_price_x96: None,
                    tick: None,
                    tick_lower: None,
                    tick_upper: None,
                    protocol,
                    exchange,
                })
            }
            "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67" => {
                let event = v3_events::Swap::decode_raw_log(topics.clone(), &data, true).ok()?;
                Some(DecodedLogEvent {
                    kind: DecodedLogKind::V3Swap,
                    pool,
                    sender: Some(format_address(event.sender)),
                    recipient: Some(format_address(event.recipient)),
                    owner: None,
                    reserve0: None,
                    reserve1: None,
                    amount0_in: None,
                    amount1_in: None,
                    amount0_out: None,
                    amount1_out: None,
                    amount0: Some(i256_to_string(event.amount0)),
                    amount1: Some(i256_to_string(event.amount1)),
                    liquidity: Some(event.liquidity as u128),
                    sqrt_price_x96: Some(u160_to_string(event.sqrtPriceX96)),
                    tick: Some(i24_to_i32(event.tick)?),
                    tick_lower: None,
                    tick_upper: None,
                    protocol,
                    exchange,
                })
            }
            sig if sig == format!("{:#x}", v3_events::Mint::SIGNATURE_HASH) => {
                let event = v3_events::Mint::decode_raw_log(topics.clone(), &data, true).ok()?;
                Some(DecodedLogEvent {
                    kind: DecodedLogKind::V3Mint,
                    pool,
                    sender: Some(format_address(event.sender)),
                    recipient: None,
                    owner: Some(format_address(event.owner)),
                    reserve0: None,
                    reserve1: None,
                    amount0_in: None,
                    amount1_in: None,
                    amount0_out: None,
                    amount1_out: None,
                    amount0: Some(u256_to_string(event.amount0)),
                    amount1: Some(u256_to_string(event.amount1)),
                    liquidity: Some(event.amount as u128),
                    sqrt_price_x96: None,
                    tick: None,
                    tick_lower: Some(i24_to_i32(event.tickLower)?),
                    tick_upper: Some(i24_to_i32(event.tickUpper)?),
                    protocol,
                    exchange,
                })
            }
            sig if sig == format!("{:#x}", v3_events::Burn::SIGNATURE_HASH) => {
                let event = v3_events::Burn::decode_raw_log(topics, &data, true).ok()?;
                Some(DecodedLogEvent {
                    kind: DecodedLogKind::V3Burn,
                    pool,
                    sender: None,
                    recipient: None,
                    owner: Some(format_address(event.owner)),
                    reserve0: None,
                    reserve1: None,
                    amount0_in: None,
                    amount1_in: None,
                    amount0_out: None,
                    amount1_out: None,
                    amount0: Some(u256_to_string(event.amount0)),
                    amount1: Some(u256_to_string(event.amount1)),
                    liquidity: Some(event.amount as u128),
                    sqrt_price_x96: None,
                    tick: None,
                    tick_lower: Some(i24_to_i32(event.tickLower)?),
                    tick_upper: Some(i24_to_i32(event.tickUpper)?),
                    protocol,
                    exchange,
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecodedLogFields {
    pub event_signature: Option<String>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
    pub decoded_event: Option<DecodedLogEvent>,
}

fn decode_topics(topics: &[String]) -> Option<Vec<B256>> {
    topics
        .iter()
        .map(|topic| topic.parse::<B256>().ok())
        .collect::<Option<Vec<_>>>()
}

fn decode_data(data: &str) -> Option<Vec<u8>> {
    let hex_data = data.strip_prefix("0x").unwrap_or(data);
    hex::decode(hex_data).ok()
}

fn u256_to_u128(value: U256) -> Option<u128> {
    value.try_into().ok()
}

fn u256_to_string(value: U256) -> String {
    value.to_string()
}

fn i256_to_string(value: I256) -> String {
    value.to_string()
}

fn u160_to_string(value: U160) -> String {
    value.to_string()
}

fn u112_to_u128(value: U112) -> u128 {
    value.try_into().expect("uint112 always fits in u128")
}

fn i24_to_i32(value: I24) -> Option<i32> {
    value.try_into().ok()
}

fn format_address(address: AlloyAddress) -> Address {
    format!("{address:#x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_log(topics: Vec<String>, data: String, address: Option<&str>) -> RawLogMessage {
        RawLogMessage {
            address: address.map(ToOwned::to_owned),
            topics,
            data: Some(data),
            tx_hash: None,
            block_hash: None,
            block_number: None,
            event_signature: None,
            protocol: None,
            exchange: None,
            log_index: None,
            removed: None,
        }
    }

    #[test]
    fn falls_back_to_first_topic_for_event_signature() {
        let message = RawLogMessage {
            address: None,
            topics: vec![
                "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822".to_string(),
            ],
            data: None,
            tx_hash: None,
            block_hash: None,
            block_number: None,
            event_signature: None,
            protocol: None,
            exchange: None,
            log_index: None,
            removed: None,
        };

        let decoded = message.decode();

        assert_eq!(
            decoded.event_signature.as_deref(),
            Some("0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822")
        );
        assert_eq!(decoded.protocol, Some(Protocol::UniswapV2));
    }

    #[test]
    fn resolves_exchange_from_log_address() {
        let message = RawLogMessage {
            address: Some("0x8c1A3cF8F83074169Fe5d7Ad50B978E1cD6b37c7".to_string()),
            topics: vec![
                "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822".to_string(),
            ],
            data: None,
            tx_hash: None,
            block_hash: None,
            block_number: None,
            event_signature: None,
            protocol: None,
            exchange: None,
            log_index: None,
            removed: None,
        };

        let decoded = message.decode();

        assert_eq!(decoded.protocol, Some(Protocol::UniswapV2));
        assert_eq!(decoded.exchange, Some(Exchange::AlienBase));
    }

    #[test]
    fn decodes_v2_sync_event() {
        let event = Sync {
            reserve0: U112::from(42u64),
            reserve1: U112::from(84u64),
        };
        let topics = event
            .encode_topics()
            .into_iter()
            .map(|topic| format!("{:#x}", B256::from(topic.0)))
            .collect();
        let data = format!("0x{}", hex::encode(event.encode_data()));
        let decoded = sample_log(topics, data, Some("0xpool")).decode();

        let event = decoded.decoded_event.unwrap();
        assert_eq!(event.kind, DecodedLogKind::V2Sync);
        assert_eq!(event.reserve0, Some(42));
        assert_eq!(event.reserve1, Some(84));
    }

    #[test]
    fn decodes_v3_swap_event() {
        let event = v3_events::Swap {
            sender: "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            recipient: "0x2222222222222222222222222222222222222222"
                .parse()
                .unwrap(),
            amount0: I256::from_raw(U256::from(10_u64)),
            amount1: I256::from_raw(U256::from(20_u64)),
            sqrtPriceX96: U160::from(30_u64),
            liquidity: 40,
            tick: I24::try_from(-5).unwrap(),
        };
        let topics = event
            .encode_topics()
            .into_iter()
            .map(|topic| format!("{:#x}", B256::from(topic.0)))
            .collect();
        let data = format!("0x{}", hex::encode(event.encode_data()));
        let decoded = sample_log(topics, data, Some("0xpool")).decode();

        let event = decoded.decoded_event.unwrap();
        assert_eq!(event.kind, DecodedLogKind::V3Swap);
        assert_eq!(event.tick, Some(-5));
        assert_eq!(event.amount0.as_deref(), Some("10"));
        assert_eq!(event.amount1.as_deref(), Some("20"));
    }
}
