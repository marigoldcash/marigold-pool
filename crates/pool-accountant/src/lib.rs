//! Turns what the bridge reports into rows: shares (M3), rewards found as
//! coinbase outputs (M4), allocation by window (M5).

mod allocator;
mod consumer;
#[cfg(test)]
mod fake_node;
mod rewards;
mod sink;

pub use allocator::{
    AllocationError, AllocationSummary, AllocatorSettings, DEFAULT_FEE_BP, DEFAULT_WINDOW_DAA, allocate_matured, run_allocator,
};
pub use consumer::{AccountantMetrics, ConsumerSettings, run_consumer};
pub use rewards::{PollError, PollSummary, WatcherSettings, is_mature, poll_once, run_reward_watcher};
pub use sink::{BroadcastSink, EVENT_CHANNEL_CAPACITY, PoolEvent};

/// A chain for the tests in this crate, so they do not depend on a backend.
#[cfg(test)]
pub(crate) mod test_chain {
    use pool_chain::Chain;

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
        async fn connect_node(_grpc_url: &str) -> Result<pool_chain::SharedNode, pool_chain::NodeError> {
            Err(pool_chain::NodeError("there is no node in the tests".into()))
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
    }
}
