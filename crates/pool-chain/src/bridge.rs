//! Starting the stratum bridge, in terms that do not name a chain. The chain's
//! backend implements `BridgeLauncher` over its own bridge.

use crate::PoolEvents;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// One stratum port, with the easiest share it hands out.
#[derive(Debug, Clone, PartialEq)]
pub struct StratumPort {
    /// Where to listen: `:5570` for every interface, or `127.0.0.1:5570`.
    pub listen: String,
    /// The lowest share difficulty on this port. A fraction is fine: a CPU at a
    /// megahash a second needs about 0.002, an ASIC sits in the thousands.
    pub min_share_diff: f64,
}

/// Everything the bridge needs to know.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeSettings {
    /// The node's gRPC address, `host:port`.
    pub node_grpc: String,
    /// The address every block pays to.
    pub pool_address: String,
    pub ports: Vec<StratumPort>,
    pub shares_per_min: u32,
    pub var_diff: bool,
    pub pow2_clamp: bool,
    pub extranonce_size: u8,
    pub block_wait_time: Duration,
    pub print_stats: bool,
}

/// Why the bridge stopped, as a sentence for the operator.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BridgeError(pub String);

#[async_trait::async_trait]
pub trait BridgeLauncher: Send + Sync {
    /// Run the bridge: connect to the node, wait for it to be in sync, listen on
    /// every port, and report every accepted share and every block to `events`.
    /// Returns when `shutdown` changes (after the ports are closed), or with an
    /// error if the bridge cannot run.
    async fn run(&self, events: Arc<dyn PoolEvents>, shutdown: watch::Receiver<bool>) -> Result<(), BridgeError>;
}

pub type SharedLauncher = Arc<dyn BridgeLauncher>;
