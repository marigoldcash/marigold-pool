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

/// The chain for this crate's tests.
#[cfg(test)]
pub(crate) mod test_chain {
    pub use pool_chain::testing::FakeChain;
}
