use types::ingest::{Exchange, Protocol};

pub fn protocol_from_event_signature(event_signature: &str) -> Option<Protocol> {
    match event_signature {
        // Uniswap V2 pair swap event.
        "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822" => {
            Some(Protocol::UniswapV2)
        }
        // Uniswap V3 pool swap event.
        "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67" => {
            Some(Protocol::UniswapV3)
        }
        _ => None,
    }
}

pub fn exchange_from_event_signature(event_signature: &str) -> Option<Exchange> {
    match event_signature {
        _ => None,
    }
}
