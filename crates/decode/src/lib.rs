use common::{
    classify_venue, router_name, ActionKind, ObservedIntent, PoolKey, RawTransactionEnvelope,
    VenueKind, WETH_BASE,
};
use ethers::abi::ParamType;
use ethers::types::{Address, Bytes, U256};
use serde_json::json;

#[derive(Default)]
pub struct IntentDecoder;

impl IntentDecoder {
    pub fn decode(&self, envelope: &RawTransactionEnvelope) -> Option<ObservedIntent> {
        let to = envelope.to?;
        let venue = classify_venue(&to);
        let raw_selector = selector_hex(&envelope.input);
        let pool_key = extract_pool_key_from_universal_router(&envelope.input);
        let decoded = decode_router_input(&envelope.input);

        let (action, token_in, token_out, amount_in) = decoded.unwrap_or((
            ActionKind::ProtocolInteraction,
            None,
            None,
            None,
        ));

        Some(ObservedIntent {
            source: envelope.source,
            tx_hash: envelope.tx_hash,
            actor: envelope.from,
            router: to,
            venue: if venue == VenueKind::Unknown && pool_key.is_some() {
                VenueKind::UniswapV4
            } else {
                venue
            },
            action,
            token_in,
            token_out,
            amount_in,
            pool_key,
            input: envelope.input.clone(),
            value: envelope.value,
            raw_selector,
            metadata: json!({
                "router_name": router_name(&to),
                "router": to,
                "value": envelope.value,
            }),
        })
    }
}

pub fn extract_pool_key_from_universal_router(input: &[u8]) -> Option<PoolKey> {
    if input.len() < 4 {
        return None;
    }

    let selector = &input[0..4];
    let param_types = if selector == [0x35, 0x93, 0x56, 0x4c] {
        vec![
            ParamType::Bytes,
            ParamType::Array(Box::new(ParamType::Bytes)),
        ]
    } else if selector == [0xca, 0xe6, 0xa6, 0xb3] {
        vec![
            ParamType::Bytes,
            ParamType::Array(Box::new(ParamType::Bytes)),
            ParamType::Uint(256),
        ]
    } else {
        return None;
    };

    let decoded = ethers::abi::decode(&param_types, &input[4..]).ok()?;
    let commands: Vec<u8> = decoded[0].clone().into_bytes()?;
    let inputs: Vec<Bytes> = decoded[1]
        .clone()
        .into_array()?
        .into_iter()
        .map(|token| Bytes::from(token.into_bytes().unwrap_or_default()))
        .collect();

    for (index, command) in commands.iter().enumerate() {
        if *command != 0x10 || index >= inputs.len() {
            continue;
        }

        let nested = ethers::abi::decode(
            &[
                ParamType::Bytes,
                ParamType::Array(Box::new(ParamType::Bytes)),
            ],
            &inputs[index],
        )
        .ok()?;

        let actions: Vec<u8> = nested[0].clone().into_bytes()?;
        let action_params: Vec<Bytes> = nested[1]
            .clone()
            .into_array()?
            .into_iter()
            .map(|token| Bytes::from(token.into_bytes().unwrap_or_default()))
            .collect();

        for (action_index, action) in actions.iter().enumerate() {
            if *action != 0x06 || action_index >= action_params.len() {
                continue;
            }

            let pool_key_type = ParamType::Tuple(vec![
                ParamType::Address,
                ParamType::Address,
                ParamType::Uint(24),
                ParamType::Int(24),
                ParamType::Address,
            ]);
            let whole_struct = ParamType::Tuple(vec![
                pool_key_type,
                ParamType::Bool,
                ParamType::Uint(128),
                ParamType::Uint(128),
                ParamType::Bytes,
            ]);

            let decoded = ethers::abi::decode(&[whole_struct], &action_params[action_index]).ok()?;
            let tuple = decoded[0].clone().into_tuple()?;
            let pool_key_tuple = tuple[0].clone().into_tuple()?;

            return Some(PoolKey {
                currency0: pool_key_tuple[0].clone().into_address()?,
                currency1: pool_key_tuple[1].clone().into_address()?,
                fee: pool_key_tuple[2].clone().into_uint()?.low_u32(),
                tick_spacing: pool_key_tuple[3].clone().into_int()?.low_u32() as i32,
                hooks: pool_key_tuple[4].clone().into_address()?,
            });
        }
    }

    None
}

fn decode_router_input(input: &[u8]) -> Option<(ActionKind, Option<Address>, Option<Address>, Option<U256>)> {
    if input.len() < 4 {
        return None;
    }

    let selector = &input[0..4];
    let read_usize = |offset: usize| -> Option<usize> {
        if offset + 32 > input.len() {
            return None;
        }
        let value = U256::from_big_endian(&input[offset..offset + 32]);
        if value > U256::from(usize::MAX) {
            return None;
        }
        Some(value.as_usize())
    };
    let read_address = |offset: usize| -> Option<Address> {
        if offset + 32 > input.len() {
            return None;
        }
        Some(Address::from_slice(&input[offset + 12..offset + 32]))
    };
    let read_uint = |offset: usize| -> Option<U256> {
        if offset + 32 > input.len() {
            return None;
        }
        Some(U256::from_big_endian(&input[offset..offset + 32]))
    };
    let get_path_token = |arg_index: usize, get_last: bool| -> Option<Address> {
        let offset_ptr = 4 + arg_index * 32;
        let array_offset = read_usize(offset_ptr)?;
        let len_ptr = 4 + array_offset;
        let array_len = read_usize(len_ptr)?;
        if array_len == 0 {
            return None;
        }
        let item_index = if get_last { array_len - 1 } else { 0 };
        let item_ptr = len_ptr + 32 + item_index * 32;
        read_address(item_ptr)
    };

    match selector {
        [0x7f, 0xf3, 0x6a, 0xb5] | [0xb6, 0xf9, 0xde, 0x95] => Some((
            ActionKind::BuyLike,
            Some(*WETH_BASE),
            get_path_token(1, true),
            read_uint(4),
        )),
        [0x18, 0xcb, 0xaf, 0xe5] | [0x79, 0x1a, 0xc9, 0x47] => Some((
            ActionKind::SellLike,
            get_path_token(2, false),
            Some(*WETH_BASE),
            read_uint(4),
        )),
        [0xf6, 0x9a, 0xc9, 0x7a] => Some((
            ActionKind::BuyLike,
            Some(*WETH_BASE),
            read_address(4),
            read_uint(36),
        )),
        [0x83, 0x1e, 0x10, 0xeb] => Some((
            ActionKind::SellLike,
            read_address(4),
            Some(*WETH_BASE),
            read_uint(36),
        )),
        [0x7b, 0xf1, 0x7d, 0x05] => {
            let routes_offset = read_usize(36)?;
            let len_ptr = 4 + routes_offset;
            let routes_len = read_usize(len_ptr)?;
            if routes_len == 0 {
                return None;
            }
            let last_route_start = len_ptr + 32 + (routes_len - 1) * 128;
            Some((
                ActionKind::BuyLike,
                Some(*WETH_BASE),
                read_address(last_route_start + 32),
                read_uint(4),
            ))
        }
        [0x45, 0x01, 0x34, 0x92] => {
            let routes_offset = read_usize(68)?;
            let len_ptr = 4 + routes_offset;
            Some((
                ActionKind::SellLike,
                read_address(len_ptr + 32),
                Some(*WETH_BASE),
                read_uint(4),
            ))
        }
        [0x38, 0xed, 0x17, 0x39] | [0x5c, 0x11, 0xd7, 0x95] => Some((
            ActionKind::Swap,
            None,
            get_path_token(2, true),
            read_uint(4),
        )),
        [0xf3, 0x05, 0xd7, 0x19] => Some((ActionKind::AddLiquidity, None, read_address(4), None)),
        [0x41, 0x4b, 0xf3, 0x89] | [0x04, 0xe4, 0x5a, 0xaf] => {
            let token_in = read_address(4)?;
            let token_out = read_address(36)?;
            let amount_in = if selector == [0x41, 0x4b, 0xf3, 0x89] {
                read_uint(164)
            } else {
                read_uint(132)
            };
            let action = if token_out == *WETH_BASE {
                ActionKind::SellLike
            } else {
                ActionKind::BuyLike
            };
            Some((action, Some(token_in), Some(token_out), amount_in))
        }
        _ => None,
    }
}

fn selector_hex(input: &[u8]) -> Option<String> {
    if input.len() < 4 {
        return None;
    }
    Some(format!("0x{}", hex::encode(&input[0..4])))
}
