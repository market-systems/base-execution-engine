use crate::raw::{as_object, string_field, u64_field};
use alloy_primitives::{hex, Address as AlloyAddress, U256};
use alloy_sol_types::{sol, SolCall};
use registry::resolve_transaction;
use serde_json::Value;
use types::ingest::{BlockContext, DecodedSwap, Exchange, Metadata, Protocol, Transaction};
use types::{Address, BlockHash, Selector, TxHash};

sol! {
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] path,
        address to,
        uint256 deadline
    );

    function swapTokensForExactTokens(
        uint256 amountOut,
        uint256 amountInMax,
        address[] path,
        address to,
        uint256 deadline
    );

    function swapExactETHForTokens(
        uint256 amountOutMin,
        address[] path,
        address to,
        uint256 deadline
    );

    function swapETHForExactTokens(
        uint256 amountOut,
        address[] path,
        address to,
        uint256 deadline
    );

    function swapExactTokensForETH(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] path,
        address to,
        uint256 deadline
    );

    function swapTokensForExactETH(
        uint256 amountOut,
        uint256 amountInMax,
        address[] path,
        address to,
        uint256 deadline
    );

    function swapExactTokensForTokensSupportingFeeOnTransferTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] path,
        address to,
        uint256 deadline
    );

    function swapExactETHForTokensSupportingFeeOnTransferTokens(
        uint256 amountOutMin,
        address[] path,
        address to,
        uint256 deadline
    );

    function swapExactTokensForETHSupportingFeeOnTransferTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] path,
        address to,
        uint256 deadline
    );

    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(ExactInputSingleParams params);

    struct ExactInputParams {
        bytes path;
        address recipient;
        uint256 deadline;
        uint256 amountIn;
        uint256 amountOutMinimum;
    }

    function exactInput(ExactInputParams params);

    struct ExactOutputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 deadline;
        uint256 amountOut;
        uint256 amountInMaximum;
        uint160 sqrtPriceLimitX96;
    }

    function exactOutputSingle(ExactOutputSingleParams params);

    struct ExactOutputParams {
        bytes path;
        address recipient;
        uint256 deadline;
        uint256 amountOut;
        uint256 amountInMaximum;
    }

    function exactOutput(ExactOutputParams params);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTransactionMessage {
    pub tx_hash: Option<TxHash>,
    pub from: Option<Address>,
    pub to: Option<Address>,
    pub value: Option<String>,
    pub input: Option<String>,
    pub block_hash: Option<BlockHash>,
    pub block_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransactionDecode {
    pub selector: Option<Selector>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
    pub decoded_swap: Option<DecodedSwap>,
}

impl RawTransactionMessage {
    pub fn from_value(value: &Value) -> Option<Self> {
        let object = as_object(value)?;

        Some(Self {
            tx_hash: string_field(object, "hash"),
            from: string_field(object, "from"),
            to: string_field(object, "to"),
            value: string_field(object, "value"),
            input: string_field(object, "input"),
            block_hash: string_field(object, "blockHash"),
            block_number: u64_field(object, "blockNumber"),
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

    pub fn decode(&self) -> TransactionDecode {
        let selector = self.selector();
        let resolution = resolve_transaction(selector.as_deref(), self.to.as_deref());
        let decoded_swap = self.decode_swap(
            selector.as_deref(),
            resolution.protocol,
            resolution.exchange,
        );

        TransactionDecode {
            protocol: resolution.protocol,
            exchange: resolution.exchange,
            selector,
            decoded_swap,
        }
    }

    pub fn selector(&self) -> Option<Selector> {
        let input = self.input.as_deref()?.trim();
        let hex = input.strip_prefix("0x").unwrap_or(input);

        if hex.len() < 8 {
            return None;
        }

        let selector = &hex[..8];
        if !selector.chars().all(|char| char.is_ascii_hexdigit()) {
            return None;
        }

        Some(format!("0x{}", selector.to_ascii_lowercase()))
    }

    pub fn to_transaction(self, metadata: Metadata) -> Transaction {
        let decoded = self.decode();

        Transaction {
            metadata,
            from: self.from,
            to: self.to,
            value: self.value,
            input: self.input,
            selector: decoded.selector,
            decoded_swap: decoded.decoded_swap,
            protocol: decoded.protocol,
            exchange: decoded.exchange,
        }
    }

    fn decode_swap(
        &self,
        selector: Option<&str>,
        protocol: Option<Protocol>,
        exchange: Option<Exchange>,
    ) -> Option<DecodedSwap> {
        let selector = selector?;
        let calldata = decode_calldata(self.input.as_deref()?)?;
        let router = self.to.clone();
        let tx_value = self.value.as_deref().and_then(parse_amount_string);

        match selector {
            "0x38ed1739" => decode_v2_exact_input::<swapExactTokensForTokensCall>(
                &calldata, router, protocol, exchange,
            ),
            "0x5c11d795" => decode_v2_exact_input::<
                swapExactTokensForTokensSupportingFeeOnTransferTokensCall,
            >(&calldata, router, protocol, exchange),
            "0x18cbafe5" => decode_v2_exact_input::<swapExactTokensForETHCall>(
                &calldata, router, protocol, exchange,
            ),
            "0x791ac947" => decode_v2_exact_input::<
                swapExactTokensForETHSupportingFeeOnTransferTokensCall,
            >(&calldata, router, protocol, exchange),
            "0x7ff36ab5" => decode_v2_exact_eth_input::<swapExactETHForTokensCall>(
                &calldata, tx_value, router, protocol, exchange,
            ),
            "0xb6f9de95" => decode_v2_exact_eth_input::<
                swapExactETHForTokensSupportingFeeOnTransferTokensCall,
            >(&calldata, tx_value, router, protocol, exchange),
            "0x8803dbee" => decode_v2_exact_output::<swapTokensForExactTokensCall>(
                &calldata, router, protocol, exchange,
            ),
            "0x4a25d94a" => decode_v2_exact_output::<swapTokensForExactETHCall>(
                &calldata, router, protocol, exchange,
            ),
            "0xfb3bdb41" => decode_v2_exact_eth_output::<swapETHForExactTokensCall>(
                &calldata, tx_value, router, protocol, exchange,
            ),
            "0x414bf389" => decode_v3_exact_input_single(&calldata, router, protocol, exchange),
            "0xc04b8d59" => decode_v3_exact_input(&calldata, router, protocol, exchange),
            "0xdb3e2198" => decode_v3_exact_output_single(&calldata, router, protocol, exchange),
            "0xf28c0498" => decode_v3_exact_output(&calldata, router, protocol, exchange),
            _ => None,
        }
    }
}

trait V2ExactInputCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, u128, Vec<AlloyAddress>, AlloyAddress)>;
}

impl V2ExactInputCall for swapExactTokensForTokensCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, u128, Vec<AlloyAddress>, AlloyAddress)> {
        let call = Self::abi_decode(data, true).ok()?;
        Some((
            u256_to_u128(call.amountIn)?,
            u256_to_u128(call.amountOutMin)?,
            call.path,
            call.to,
        ))
    }
}

impl V2ExactInputCall for swapExactTokensForTokensSupportingFeeOnTransferTokensCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, u128, Vec<AlloyAddress>, AlloyAddress)> {
        let call = Self::abi_decode(data, true).ok()?;
        Some((
            u256_to_u128(call.amountIn)?,
            u256_to_u128(call.amountOutMin)?,
            call.path,
            call.to,
        ))
    }
}

impl V2ExactInputCall for swapExactTokensForETHCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, u128, Vec<AlloyAddress>, AlloyAddress)> {
        let call = Self::abi_decode(data, true).ok()?;
        Some((
            u256_to_u128(call.amountIn)?,
            u256_to_u128(call.amountOutMin)?,
            call.path,
            call.to,
        ))
    }
}

impl V2ExactInputCall for swapExactTokensForETHSupportingFeeOnTransferTokensCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, u128, Vec<AlloyAddress>, AlloyAddress)> {
        let call = Self::abi_decode(data, true).ok()?;
        Some((
            u256_to_u128(call.amountIn)?,
            u256_to_u128(call.amountOutMin)?,
            call.path,
            call.to,
        ))
    }
}

trait V2ExactEthInputCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, Vec<AlloyAddress>, AlloyAddress)>;
}

impl V2ExactEthInputCall for swapExactETHForTokensCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, Vec<AlloyAddress>, AlloyAddress)> {
        let call = Self::abi_decode(data, true).ok()?;
        Some((u256_to_u128(call.amountOutMin)?, call.path, call.to))
    }
}

impl V2ExactEthInputCall for swapExactETHForTokensSupportingFeeOnTransferTokensCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, Vec<AlloyAddress>, AlloyAddress)> {
        let call = Self::abi_decode(data, true).ok()?;
        Some((u256_to_u128(call.amountOutMin)?, call.path, call.to))
    }
}

trait V2ExactOutputCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, u128, Vec<AlloyAddress>, AlloyAddress)>;
}

impl V2ExactOutputCall for swapTokensForExactTokensCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, u128, Vec<AlloyAddress>, AlloyAddress)> {
        let call = Self::abi_decode(data, true).ok()?;
        Some((
            u256_to_u128(call.amountOut)?,
            u256_to_u128(call.amountInMax)?,
            call.path,
            call.to,
        ))
    }
}

impl V2ExactOutputCall for swapTokensForExactETHCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, u128, Vec<AlloyAddress>, AlloyAddress)> {
        let call = Self::abi_decode(data, true).ok()?;
        Some((
            u256_to_u128(call.amountOut)?,
            u256_to_u128(call.amountInMax)?,
            call.path,
            call.to,
        ))
    }
}

trait V2ExactEthOutputCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, Vec<AlloyAddress>, AlloyAddress)>;
}

impl V2ExactEthOutputCall for swapETHForExactTokensCall {
    fn abi_decode_swap(data: &[u8]) -> Option<(u128, Vec<AlloyAddress>, AlloyAddress)> {
        let call = Self::abi_decode(data, true).ok()?;
        Some((u256_to_u128(call.amountOut)?, call.path, call.to))
    }
}

fn decode_v2_exact_input<T: V2ExactInputCall>(
    calldata: &[u8],
    router: Option<Address>,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
) -> Option<DecodedSwap> {
    let (amount_in, amount_out_minimum, path, recipient) = T::abi_decode_swap(calldata)?;
    let (token_in, token_out) = decode_v2_path(&path)?;

    Some(DecodedSwap {
        pool: None,
        router,
        token_in: Some(token_in),
        token_out: Some(token_out),
        amount_in: Some(amount_in),
        amount_out: None,
        amount_in_maximum: None,
        amount_out_minimum: Some(amount_out_minimum),
        exact_input: Some(true),
        recipient: Some(format_address(recipient)),
        protocol,
        exchange,
    })
}

fn decode_v2_exact_eth_input<T: V2ExactEthInputCall>(
    calldata: &[u8],
    tx_value: Option<u128>,
    router: Option<Address>,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
) -> Option<DecodedSwap> {
    let (amount_out_minimum, path, recipient) = T::abi_decode_swap(calldata)?;
    let (token_in, token_out) = decode_v2_path(&path)?;

    Some(DecodedSwap {
        pool: None,
        router,
        token_in: Some(token_in),
        token_out: Some(token_out),
        amount_in: tx_value,
        amount_out: None,
        amount_in_maximum: None,
        amount_out_minimum: Some(amount_out_minimum),
        exact_input: Some(true),
        recipient: Some(format_address(recipient)),
        protocol,
        exchange,
    })
}

fn decode_v2_exact_output<T: V2ExactOutputCall>(
    calldata: &[u8],
    router: Option<Address>,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
) -> Option<DecodedSwap> {
    let (amount_out, amount_in_maximum, path, recipient) = T::abi_decode_swap(calldata)?;
    let (token_in, token_out) = decode_v2_path(&path)?;

    Some(DecodedSwap {
        pool: None,
        router,
        token_in: Some(token_in),
        token_out: Some(token_out),
        amount_in: None,
        amount_out: Some(amount_out),
        amount_in_maximum: Some(amount_in_maximum),
        amount_out_minimum: None,
        exact_input: Some(false),
        recipient: Some(format_address(recipient)),
        protocol,
        exchange,
    })
}

fn decode_v2_exact_eth_output<T: V2ExactEthOutputCall>(
    calldata: &[u8],
    tx_value: Option<u128>,
    router: Option<Address>,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
) -> Option<DecodedSwap> {
    let (amount_out, path, recipient) = T::abi_decode_swap(calldata)?;
    let (token_in, token_out) = decode_v2_path(&path)?;

    Some(DecodedSwap {
        pool: None,
        router,
        token_in: Some(token_in),
        token_out: Some(token_out),
        amount_in: None,
        amount_out: Some(amount_out),
        amount_in_maximum: tx_value,
        amount_out_minimum: None,
        exact_input: Some(false),
        recipient: Some(format_address(recipient)),
        protocol,
        exchange,
    })
}

fn decode_v3_exact_input_single(
    calldata: &[u8],
    router: Option<Address>,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
) -> Option<DecodedSwap> {
    let call = exactInputSingleCall::abi_decode(calldata, true).ok()?;

    Some(DecodedSwap {
        pool: None,
        router,
        token_in: Some(format_address(call.params.tokenIn)),
        token_out: Some(format_address(call.params.tokenOut)),
        amount_in: Some(u256_to_u128(call.params.amountIn)?),
        amount_out: None,
        amount_in_maximum: None,
        amount_out_minimum: Some(u256_to_u128(call.params.amountOutMinimum)?),
        exact_input: Some(true),
        recipient: Some(format_address(call.params.recipient)),
        protocol,
        exchange,
    })
}

fn decode_v3_exact_input(
    calldata: &[u8],
    router: Option<Address>,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
) -> Option<DecodedSwap> {
    let call = exactInputCall::abi_decode(calldata, true).ok()?;
    let (token_in, token_out) = decode_v3_path(&call.params.path, false)?;

    Some(DecodedSwap {
        pool: None,
        router,
        token_in: Some(token_in),
        token_out: Some(token_out),
        amount_in: Some(u256_to_u128(call.params.amountIn)?),
        amount_out: None,
        amount_in_maximum: None,
        amount_out_minimum: Some(u256_to_u128(call.params.amountOutMinimum)?),
        exact_input: Some(true),
        recipient: Some(format_address(call.params.recipient)),
        protocol,
        exchange,
    })
}

fn decode_v3_exact_output_single(
    calldata: &[u8],
    router: Option<Address>,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
) -> Option<DecodedSwap> {
    let call = exactOutputSingleCall::abi_decode(calldata, true).ok()?;

    Some(DecodedSwap {
        pool: None,
        router,
        token_in: Some(format_address(call.params.tokenIn)),
        token_out: Some(format_address(call.params.tokenOut)),
        amount_in: None,
        amount_out: Some(u256_to_u128(call.params.amountOut)?),
        amount_in_maximum: Some(u256_to_u128(call.params.amountInMaximum)?),
        amount_out_minimum: None,
        exact_input: Some(false),
        recipient: Some(format_address(call.params.recipient)),
        protocol,
        exchange,
    })
}

fn decode_v3_exact_output(
    calldata: &[u8],
    router: Option<Address>,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
) -> Option<DecodedSwap> {
    let call = exactOutputCall::abi_decode(calldata, true).ok()?;
    let (token_in, token_out) = decode_v3_path(&call.params.path, true)?;

    Some(DecodedSwap {
        pool: None,
        router,
        token_in: Some(token_in),
        token_out: Some(token_out),
        amount_in: None,
        amount_out: Some(u256_to_u128(call.params.amountOut)?),
        amount_in_maximum: Some(u256_to_u128(call.params.amountInMaximum)?),
        amount_out_minimum: None,
        exact_input: Some(false),
        recipient: Some(format_address(call.params.recipient)),
        protocol,
        exchange,
    })
}

fn decode_calldata(input: &str) -> Option<Vec<u8>> {
    let hex = input.trim().strip_prefix("0x").unwrap_or(input.trim());
    hex::decode(hex).ok()
}

fn parse_amount_string(raw: &str) -> Option<u128> {
    if let Some(hex) = raw.strip_prefix("0x") {
        u128::from_str_radix(hex, 16).ok()
    } else {
        raw.parse::<u128>().ok()
    }
}

fn u256_to_u128(value: U256) -> Option<u128> {
    value.try_into().ok()
}

fn decode_v2_path(path: &[AlloyAddress]) -> Option<(Address, Address)> {
    let first = path.first().copied()?;
    let last = path.last().copied()?;
    Some((format_address(first), format_address(last)))
}

fn decode_v3_path(path: &[u8], reverse: bool) -> Option<(Address, Address)> {
    if path.len() < 43 || !(path.len() - 20).is_multiple_of(23) {
        return None;
    }

    let first = AlloyAddress::from_slice(path.get(0..20)?);
    let last_start = path.len().checked_sub(20)?;
    let last = AlloyAddress::from_slice(path.get(last_start..)?);

    if reverse {
        Some((format_address(last), format_address(first)))
    } else {
        Some((format_address(first), format_address(last)))
    }
}

fn format_address(address: AlloyAddress) -> Address {
    format!("{address:#x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{aliases::U24, U160};

    fn sample_message(input: String, value: Option<&str>, to: &str) -> RawTransactionMessage {
        RawTransactionMessage {
            tx_hash: None,
            from: None,
            to: Some(to.to_string()),
            value: value.map(ToOwned::to_owned),
            input: Some(input),
            block_hash: None,
            block_number: None,
        }
    }

    #[test]
    fn extracts_selector_from_input() {
        let message = sample_message(
            "0x38ed173900000000000000000000000000000000000000000000000000000000".to_string(),
            None,
            "0x4752ba5dbc23f44d87826276bf6fd6b1c372ad24",
        );

        assert_eq!(message.selector().as_deref(), Some("0x38ed1739"));
    }

    #[test]
    fn resolves_exchange_from_address_when_selector_is_generic() {
        let message = sample_message(
            "0x414bf38900000000".to_string(),
            None,
            "0x678Aa4bF4E210cf2166753e054d5b7c31cc7fa86",
        );

        let decoded = message.decode();

        assert_eq!(decoded.selector.as_deref(), Some("0x414bf389"));
        assert_eq!(decoded.protocol, Some(Protocol::UniswapV3));
        assert_eq!(decoded.exchange, Some(Exchange::PancakeSwap));
    }

    #[test]
    fn decodes_v2_exact_input_swap() {
        let call = swapExactTokensForTokensCall {
            amountIn: U256::from(123_u64),
            amountOutMin: U256::from(456_u64),
            path: vec![
                "0x1111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
                "0x2222222222222222222222222222222222222222"
                    .parse()
                    .unwrap(),
            ],
            to: "0x3333333333333333333333333333333333333333"
                .parse()
                .unwrap(),
            deadline: U256::from(999_u64),
        };
        let calldata = format!("0x{}", hex::encode(call.abi_encode()));
        let message = sample_message(calldata, None, "0x4752ba5dbc23f44d87826276bf6fd6b1c372ad24");

        let decoded = message.decode().decoded_swap.unwrap();

        assert_eq!(
            decoded.token_in.as_deref(),
            Some("0x1111111111111111111111111111111111111111")
        );
        assert_eq!(
            decoded.token_out.as_deref(),
            Some("0x2222222222222222222222222222222222222222")
        );
        assert_eq!(decoded.amount_in, Some(123));
        assert_eq!(decoded.amount_out_minimum, Some(456));
        assert_eq!(decoded.exact_input, Some(true));
    }

    #[test]
    fn decodes_v3_exact_output_single_swap() {
        let call = exactOutputSingleCall {
            params: ExactOutputSingleParams {
                tokenIn: "0x1111111111111111111111111111111111111111"
                    .parse()
                    .unwrap(),
                tokenOut: "0x2222222222222222222222222222222222222222"
                    .parse()
                    .unwrap(),
                fee: U24::from(3_000u64),
                recipient: "0x3333333333333333333333333333333333333333"
                    .parse()
                    .unwrap(),
                deadline: U256::from(10_u64),
                amountOut: U256::from(20_u64),
                amountInMaximum: U256::from(30_u64),
                sqrtPriceLimitX96: U160::ZERO,
            },
        };
        let calldata = format!("0x{}", hex::encode(call.abi_encode()));
        let message = sample_message(calldata, None, "0x2626664c2603336e57b271c5c0b26f421741e481");

        let decoded = message.decode().decoded_swap.unwrap();

        assert_eq!(decoded.amount_out, Some(20));
        assert_eq!(decoded.amount_in_maximum, Some(30));
        assert_eq!(decoded.exact_input, Some(false));
        assert_eq!(decoded.protocol, Some(Protocol::UniswapV3));
    }

    #[test]
    fn decodes_v3_exact_input_path_endpoints() {
        let path = hex::decode(concat!(
            "1111111111111111111111111111111111111111",
            "000bb8",
            "2222222222222222222222222222222222222222",
            "0001f4",
            "3333333333333333333333333333333333333333"
        ))
        .unwrap();
        let call = exactInputCall {
            params: ExactInputParams {
                path: path.into(),
                recipient: "0x4444444444444444444444444444444444444444"
                    .parse()
                    .unwrap(),
                deadline: U256::from(10_u64),
                amountIn: U256::from(20_u64),
                amountOutMinimum: U256::from(30_u64),
            },
        };
        let calldata = format!("0x{}", hex::encode(call.abi_encode()));
        let message = sample_message(calldata, None, "0x2626664c2603336e57b271c5c0b26f421741e481");

        let decoded = message.decode().decoded_swap.unwrap();

        assert_eq!(
            decoded.token_in.as_deref(),
            Some("0x1111111111111111111111111111111111111111")
        );
        assert_eq!(
            decoded.token_out.as_deref(),
            Some("0x3333333333333333333333333333333333333333")
        );
        assert_eq!(decoded.amount_in, Some(20));
        assert_eq!(decoded.amount_out_minimum, Some(30));
    }
}
