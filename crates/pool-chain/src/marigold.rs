//! The Marigold backend. Ports, prefixes and maturity come from the node's own
//! network definitions rather than being repeated here, so they cannot drift
//! from the node this pool is pinned to.

use crate::{BlockReport, Chain, NodeError, NodeUtxo, NodeView, PoolEvents, ShareReport, SharedNode};
use kaspa_addresses::{Address, Prefix};
use kaspa_consensus_core::config::params::Params;
use kaspa_consensus_core::network::{NetworkId, NetworkType};
use kaspa_grpc_client::GrpcClient;
use kaspa_hashes::Hash;
use kaspa_rpc_core::api::rpc::RpcApi;
use kaspa_rpc_core::notify::mode::NotificationMode;
use std::str::FromStr;
use std::sync::Arc;

pub struct MarigoldChain;

impl MarigoldChain {
    /// The sink to hand the Marigold stratum bridge: it turns the bridge's own
    /// share and block events into the chain-neutral reports and passes them on.
    pub fn bridge_sink(events: Arc<dyn PoolEvents>) -> Arc<dyn kaspa_stratum_bridge::PoolSink> {
        Arc::new(BridgeSink { events })
    }
}

struct BridgeSink {
    events: Arc<dyn PoolEvents>,
}

impl kaspa_stratum_bridge::PoolSink for BridgeSink {
    fn share_accepted(&self, event: kaspa_stratum_bridge::ShareEvent) {
        self.events.share_accepted(ShareReport {
            address: event.address,
            worker: event.worker,
            difficulty: event.difficulty,
            hash_value: event.hash_value,
            daa_score: event.daa_score,
            blue_score: event.blue_score,
            timestamp_ms: event.timestamp_ms,
        });
    }

    fn block_found(&self, event: kaspa_stratum_bridge::BlockEvent) {
        self.events.block_found(BlockReport {
            address: event.address,
            worker: event.worker,
            hash: event.hash,
            blue_score: event.blue_score,
            daa_score: event.daa_score,
            nonce: event.nonce,
            timestamp_ms: event.timestamp_ms,
        });
    }
}

/// The network types the pool supports: mainnet, testnet-10, and the two local
/// development networks. Any other id (an unknown word, `testnet-11`,
/// `mainnet-3`) is not one of them.
fn network_type(id: &str) -> Option<NetworkType> {
    let id = NetworkId::from_str(id).ok()?;
    match (id.network_type(), id.suffix()) {
        (NetworkType::Testnet, Some(10)) => Some(NetworkType::Testnet),
        (NetworkType::Testnet, _) => None,
        (other, None) => Some(other),
        (_, Some(_)) => None,
    }
}

/// The node, over gRPC. The client reconnects by itself if the node restarts.
struct GrpcNode {
    client: GrpcClient,
}

fn node_error(what: &str, error: impl std::fmt::Display) -> NodeError {
    NodeError(format!("The node did not answer {what}: {error}"))
}

#[async_trait::async_trait]
impl NodeView for GrpcNode {
    async fn virtual_daa_score(&self) -> Result<u64, NodeError> {
        let info = self.client.get_block_dag_info().await.map_err(|e| node_error("the request for its current DAA score", e))?;
        Ok(info.virtual_daa_score)
    }

    async fn utxos_at(&self, address: &str) -> Result<Vec<NodeUtxo>, NodeError> {
        let parsed = Address::try_from(address).map_err(|e| NodeError(format!("{address:?} is not an address: {e}")))?;
        let entries = self
            .client
            .get_utxos_by_addresses(vec![parsed])
            .await
            .map_err(|e| node_error("the request for an address's outputs", e))?;
        Ok(entries
            .into_iter()
            .map(|entry| NodeUtxo {
                outpoint: format!("{}:{}", entry.outpoint.transaction_id, entry.outpoint.index),
                amount: entry.utxo_entry.amount,
                block_daa_score: entry.utxo_entry.block_daa_score,
                is_coinbase: entry.utxo_entry.is_coinbase,
            })
            .collect())
    }

    async fn balance_at(&self, address: &str) -> Result<u64, NodeError> {
        let parsed = Address::try_from(address).map_err(|e| NodeError(format!("{address:?} is not an address: {e}")))?;
        self.client.get_balance_by_address(parsed).await.map_err(|e| node_error("the request for an address's balance", e))
    }

    async fn block_is_blue(&self, hash: &str) -> Result<bool, NodeError> {
        let parsed = Hash::from_str(hash).map_err(|e| NodeError(format!("{hash:?} is not a block hash: {e}")))?;
        let colour =
            self.client.get_current_block_color(parsed).await.map_err(|e| node_error("the request for a block's colour", e))?;
        Ok(colour.blue)
    }
}

fn describe(network: NetworkType) -> &'static str {
    match network {
        NetworkType::Mainnet => "the main network",
        NetworkType::Testnet => "the test network",
        NetworkType::Devnet => "the development network",
        NetworkType::Simnet => "the simulation network",
    }
}

fn prefix_str(prefix: Prefix) -> &'static str {
    match prefix {
        Prefix::Mainnet => "marigold",
        Prefix::Testnet => "marigoldtest",
        Prefix::Simnet => "marigoldsim",
        Prefix::Devnet => "marigolddev",
        #[allow(unreachable_patterns)]
        _ => "marigold",
    }
}

impl Chain for MarigoldChain {
    fn ticker() -> &'static str {
        "MAGLD"
    }

    fn unit_name() -> &'static str {
        "petal"
    }

    fn address_prefix(network: &str) -> Option<&'static str> {
        network_type(network).map(|n| prefix_str(Prefix::from(n)))
    }

    fn default_grpc_port(network: &str) -> Option<u16> {
        network_type(network).map(|n| n.default_rpc_port())
    }

    fn default_wrpc_port(network: &str) -> Option<u16> {
        network_type(network).map(|n| n.default_borsh_rpc_port())
    }

    fn coinbase_maturity_daa(network: &str) -> Option<u64> {
        network_type(network).map(|n| Params::from(n).coinbase_maturity())
    }

    fn validate_address(network: &str, text: &str) -> Result<(), String> {
        let Some(kind) = network_type(network) else {
            return Err(format!(
                "The pool is set to a network called \"{network}\", which Marigold does not have. Use mainnet or testnet-10 in the pool's settings."
            ));
        };
        let wanted = Prefix::from(kind);
        let Ok(address) = Address::try_from(text) else {
            return Err(format!(
                "That does not look like a Marigold address. It starts with {}: on {}.",
                prefix_str(wanted),
                describe(kind)
            ));
        };
        if address.prefix != wanted {
            return Err(format!(
                "That address is for a different network. On {}, addresses start with {}:.",
                describe(kind),
                prefix_str(wanted)
            ));
        }
        Ok(())
    }

    async fn connect_node(grpc_url: &str) -> Result<SharedNode, NodeError> {
        let url = if grpc_url.starts_with("grpc://") { grpc_url.to_owned() } else { format!("grpc://{grpc_url}") };
        let client = GrpcClient::connect_with_args(
            NotificationMode::Direct,
            url.clone(),
            None,
            true,
            None,
            false,
            Some(500_000),
            Default::default(),
        )
        .await
        .map_err(|e| NodeError(format!("Could not reach the node at {url}: {e}")))?;
        Ok(Arc::new(GrpcNode { client }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_addresses::Version;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder {
        shares: Mutex<Vec<ShareReport>>,
        blocks: Mutex<Vec<BlockReport>>,
    }

    impl PoolEvents for Recorder {
        fn share_accepted(&self, report: ShareReport) {
            self.shares.lock().unwrap().push(report);
        }
        fn block_found(&self, report: BlockReport) {
            self.blocks.lock().unwrap().push(report);
        }
    }

    #[test]
    fn the_bridges_share_event_arrives_as_a_report_with_every_field_intact() {
        let recorder = Arc::new(Recorder::default());
        let sink = MarigoldChain::bridge_sink(recorder.clone());
        sink.share_accepted(kaspa_stratum_bridge::ShareEvent {
            address: "marigoldtest:abc".into(),
            worker: "asic1".into(),
            difficulty: 2048.5,
            hash_value: 4_194_304.25,
            daa_score: 111,
            blue_score: 222,
            timestamp_ms: 333,
        });
        let got = recorder.shares.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![ShareReport {
                address: "marigoldtest:abc".into(),
                worker: "asic1".into(),
                difficulty: 2048.5,
                hash_value: 4_194_304.25,
                daa_score: 111,
                blue_score: 222,
                timestamp_ms: 333,
            }]
        );
        assert!(recorder.blocks.lock().unwrap().is_empty());
    }

    #[test]
    fn the_bridges_block_event_arrives_as_a_report_with_every_field_intact() {
        let recorder = Arc::new(Recorder::default());
        let sink = MarigoldChain::bridge_sink(recorder.clone());
        sink.block_found(kaspa_stratum_bridge::BlockEvent {
            address: "marigoldtest:abc".into(),
            worker: "asic1".into(),
            hash: "ab12".into(),
            blue_score: 5,
            daa_score: 6,
            nonce: u64::MAX,
            timestamp_ms: 7,
        });
        let got = recorder.blocks.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![BlockReport {
                address: "marigoldtest:abc".into(),
                worker: "asic1".into(),
                hash: "ab12".into(),
                blue_score: 5,
                daa_score: 6,
                nonce: u64::MAX,
                timestamp_ms: 7,
            }]
        );
        assert!(recorder.shares.lock().unwrap().is_empty());
    }

    fn address(prefix: Prefix) -> String {
        Address::new(prefix, Version::PubKey, &[7u8; 32]).to_string()
    }

    #[test]
    fn the_names_are_marigolds() {
        assert_eq!(MarigoldChain::ticker(), "MAGLD");
        assert_eq!(MarigoldChain::unit_name(), "petal");
    }

    #[test]
    fn prefixes_follow_the_network() {
        assert_eq!(MarigoldChain::address_prefix("mainnet"), Some("marigold"));
        assert_eq!(MarigoldChain::address_prefix("testnet-10"), Some("marigoldtest"));
        assert_eq!(MarigoldChain::address_prefix("testnet"), Some("marigoldtest"), "a bare testnet means testnet-10");
    }

    #[test]
    fn ports_are_the_nodes_defaults() {
        assert_eq!(MarigoldChain::default_grpc_port("mainnet"), Some(26110));
        assert_eq!(MarigoldChain::default_grpc_port("testnet-10"), Some(26210));
        assert_eq!(MarigoldChain::default_wrpc_port("mainnet"), Some(27110));
        assert_eq!(MarigoldChain::default_wrpc_port("testnet-10"), Some(27210));
    }

    #[test]
    fn a_coinbase_waits_a_thousand_steps() {
        assert_eq!(MarigoldChain::coinbase_maturity_daa("mainnet"), Some(1000));
        assert_eq!(MarigoldChain::coinbase_maturity_daa("testnet-10"), Some(1000));
    }

    #[test]
    fn networks_the_chain_does_not_have_are_refused() {
        for bad in ["", "kaspa", "testnet-11", "mainnet-3", "testnet-x", "mainnet-testnet-10"] {
            assert_eq!(MarigoldChain::address_prefix(bad), None, "{bad:?}");
            assert_eq!(MarigoldChain::default_grpc_port(bad), None, "{bad:?}");
            assert_eq!(MarigoldChain::coinbase_maturity_daa(bad), None, "{bad:?}");
        }
        let error = MarigoldChain::validate_address("testnet-11", &address(Prefix::Testnet)).unwrap_err();
        assert!(error.contains("testnet-11") && error.contains("testnet-10"), "{error}");
    }

    #[test]
    fn an_address_on_the_right_network_passes() {
        assert_eq!(MarigoldChain::validate_address("testnet-10", &address(Prefix::Testnet)), Ok(()));
        assert_eq!(MarigoldChain::validate_address("mainnet", &address(Prefix::Mainnet)), Ok(()));
    }

    #[test]
    fn an_address_on_the_wrong_network_says_so_in_words() {
        let error = MarigoldChain::validate_address("testnet-10", &address(Prefix::Mainnet)).unwrap_err();
        assert_eq!(error, "That address is for a different network. On the test network, addresses start with marigoldtest:.");
    }

    #[test]
    fn something_that_is_not_an_address_says_what_one_looks_like() {
        for junk in ["", "hello", "marigoldtest:nonsense", "kaspa:qqqqqqqq"] {
            let error = MarigoldChain::validate_address("testnet-10", junk).unwrap_err();
            assert_eq!(
                error, "That does not look like a Marigold address. It starts with marigoldtest: on the test network.",
                "{junk:?}"
            );
        }
    }
}
