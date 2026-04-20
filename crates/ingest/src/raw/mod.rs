mod block;
mod log;
mod transaction;

use serde_json::{Map, Value};

pub use block::RawBlockMessage;
pub use log::{DecodedLogFields, RawLogMessage};
pub use transaction::{RawTransactionMessage, TransactionDecode};

pub(crate) fn as_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

pub(crate) fn string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key)?.as_str().map(ToOwned::to_owned)
}

pub(crate) fn bool_field(object: &Map<String, Value>, key: &str) -> Option<bool> {
    object.get(key)?.as_bool()
}

pub(crate) fn u64_field(object: &Map<String, Value>, key: &str) -> Option<u64> {
    let value = object.get(key)?;

    match value {
        Value::String(raw) => parse_u64(raw),
        Value::Number(number) => number.as_u64(),
        _ => None,
    }
}

fn parse_u64(raw: &str) -> Option<u64> {
    if let Some(hex) = raw.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else {
        raw.parse::<u64>().ok()
    }
}
