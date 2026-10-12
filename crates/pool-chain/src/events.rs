//! What the bridge tells the pool, in terms that do not name a chain. The
//! chain's backend adapts its own bridge's events to this (see
//! `MarigoldChain::bridge_sink`), and the pool's accountant listens through it.

/// A share the bridge accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct ShareReport {
    /// The address the miner logged in with, exactly as the bridge saw it.
    pub address: String,
    pub worker: String,
    /// The stratum difficulty the share was checked against.
    pub difficulty: f64,
    /// The expected number of hashes the share stands for.
    pub hash_value: f64,
    pub daa_score: u64,
    pub blue_score: u64,
    pub timestamp_ms: u64,
}

/// A block the node accepted from one of the pool's miners.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockReport {
    pub address: String,
    pub worker: String,
    pub hash: String,
    pub blue_score: u64,
    pub daa_score: u64,
    pub nonce: u64,
    pub timestamp_ms: u64,
}

/// The pool's ear on the bridge. Both calls happen inline on the bridge's
/// submit path, so an implementation must not block: push onto a channel and
/// return.
pub trait PoolEvents: Send + Sync {
    fn share_accepted(&self, report: ShareReport);
    fn block_found(&self, report: BlockReport);
}
