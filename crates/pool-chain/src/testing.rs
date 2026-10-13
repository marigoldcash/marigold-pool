//! A chain that exists only in tests, so the crates above `pool-chain` can be
//! tested without a real backend. Turned on with the `testing` feature.

use crate::{
    BridgeError, BridgeLauncher, BridgeSettings, Chain, NodeError, PayError, PoolEvents, SharedLauncher, SharedNode, SharedPayer,
    WalletCreated, WalletSettings,
};
use std::sync::Arc;
use tokio::sync::watch;

/// Addresses look like `fake:abc123` on the network "fakenet". The ticker is
/// TST and the smallest unit is the grain.
pub struct FakeChain;

impl Chain for FakeChain {
    fn ticker() -> &'static str {
        "TST"
    }

    fn unit_name() -> &'static str {
        "grain"
    }

    fn address_prefix(network: &str) -> Option<&'static str> {
        (network == "fakenet").then_some("fake")
    }

    fn default_grpc_port(network: &str) -> Option<u16> {
        (network == "fakenet").then_some(1)
    }

    fn default_wrpc_port(network: &str) -> Option<u16> {
        (network == "fakenet").then_some(2)
    }

    fn coinbase_maturity_daa(network: &str) -> Option<u64> {
        (network == "fakenet").then_some(1000)
    }

    fn validate_address(network: &str, text: &str) -> Result<(), String> {
        if network != "fakenet" {
            return Err("This network does not exist.".into());
        }
        match text.strip_prefix("fake:") {
            Some(rest) if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric()) => Ok(()),
            _ => Err("That does not look like an address. It starts with fake:.".into()),
        }
    }

    async fn connect_node(_grpc_url: &str) -> Result<SharedNode, NodeError> {
        Err(NodeError("there is no node in the tests".into()))
    }

    async fn init_wallet(_settings: &WalletSettings) -> Result<WalletCreated, PayError> {
        Err(PayError("there is no wallet in the tests".into()))
    }

    fn bridge_launcher(_settings: BridgeSettings) -> SharedLauncher {
        Arc::new(NoBridge)
    }

    async fn open_payer(_settings: &WalletSettings) -> Result<SharedPayer, PayError> {
        Err(PayError("there is no wallet in the tests".into()))
    }
}

/// A bridge that is not there.
struct NoBridge;

#[async_trait::async_trait]
impl BridgeLauncher for NoBridge {
    async fn run(&self, _events: Arc<dyn PoolEvents>, _shutdown: watch::Receiver<bool>) -> Result<(), BridgeError> {
        Err(BridgeError("there is no bridge in the tests".into()))
    }
}
