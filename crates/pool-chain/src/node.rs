//! What the pool asks its node, in terms that do not name a chain. The chain's
//! backend implements `NodeView` over its own node's RPC.

use std::sync::Arc;

/// An unspent output at an address, as the node reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeUtxo {
    /// `transaction id:output index`, which is unique on the chain.
    pub outpoint: String,
    pub amount: u64,
    /// The DAA score at which this output was created.
    pub block_daa_score: u64,
    /// Whether it is a block reward rather than an ordinary payment.
    pub is_coinbase: bool,
}

/// Why the node could not answer, as a sentence for the operator's log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NodeError(pub String);

#[async_trait::async_trait]
pub trait NodeView: Send + Sync {
    /// The DAA score of the node's current tip, which is how time is measured
    /// for coinbase maturity.
    async fn virtual_daa_score(&self) -> Result<u64, NodeError>;

    /// Every unspent output at `address`.
    async fn utxos_at(&self, address: &str) -> Result<Vec<NodeUtxo>, NodeError>;

    /// The total the node counts at `address`: every unspent output, rewards
    /// still waiting to mature included.
    async fn balance_at(&self, address: &str) -> Result<u64, NodeError>;

    /// Whether a block is blue in the node's current view of the DAG. An error
    /// means it is not known (yet), or that the node could not be reached.
    async fn block_is_blue(&self, hash: &str) -> Result<bool, NodeError>;
}

/// A handle every task can share.
pub type SharedNode = Arc<dyn NodeView>;
