use types::ingest::{Exchange, Protocol};

pub fn protocol_from_selector(selector: &str) -> Option<Protocol> {
    match selector {
        // Uniswap V2 / Router02 family swap entrypoints.
        "0x38ed1739" | "0x18cbafe5" | "0x7ff36ab5" | "0x8803dbee" => {
            Some(Protocol::UniswapV2)
        }
        // Uniswap V3 swap router `exactInputSingle`.
        "0x414bf389" => Some(Protocol::UniswapV3),
        // Universal Router `execute`.
        "0x3593564c" => Some(Protocol::UniswapV4),
        _ => None,
    }
}

pub fn exchange_from_selector(selector: &str) -> Option<Exchange> {
    match selector {
        _ => None,
    }
}
