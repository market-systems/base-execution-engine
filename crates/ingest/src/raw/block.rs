use crate::raw::{as_object, string_field, u64_field};
use serde_json::Value;
use types::ingest::{Block, BlockContext, Metadata};
use types::BlockHash;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawBlockMessage {
    pub block_hash: Option<BlockHash>,
    pub parent_hash: Option<BlockHash>,
    pub block_number: Option<u64>,
    pub timestamp_secs: Option<u64>,
}

impl RawBlockMessage {
    pub fn from_value(value: &Value) -> Option<Self> {
        let object = as_object(value)?;

        Some(Self {
            block_hash: string_field(object, "hash"),
            parent_hash: string_field(object, "parentHash"),
            block_number: u64_field(object, "number"),
            timestamp_secs: u64_field(object, "timestamp"),
        })
    }

    pub fn block_context(&self) -> BlockContext {
        BlockContext::Block {
            number: self.block_number.unwrap_or_default(),
            hash: self.block_hash.clone(),
        }
    }

    pub fn to_block(self, metadata: Metadata) -> Block {
        Block {
            metadata,
            block_hash: self.block_hash,
            parent_hash: self.parent_hash,
            timestamp_secs: self.timestamp_secs,
        }
    }
}
