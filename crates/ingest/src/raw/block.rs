use types::ingest::{Block, Metadata};
use types::BlockHash;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawBlockMessage {
    pub block_hash: Option<BlockHash>,
    pub parent_hash: Option<BlockHash>,
    pub timestamp_secs: Option<u64>,
}

impl RawBlockMessage {
    pub fn to_block(self, metadata: Metadata) -> Block {
        Block {
            metadata,
            block_hash: self.block_hash,
            parent_hash: self.parent_hash,
            timestamp_secs: self.timestamp_secs,
        }
    }
}
