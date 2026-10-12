use pool_chain::{BlockReport, PoolEvents, ShareReport};
use tokio::sync::broadcast;

/// How many events the channel between the bridge and the consumer holds. A
/// consumer that falls this far behind loses the oldest events (and says so).
pub const EVENT_CHANNEL_CAPACITY: usize = 4096;

/// What travels from the bridge to the accountant.
#[derive(Debug, Clone, PartialEq)]
pub enum PoolEvent {
    Share(ShareReport),
    Block(BlockReport),
}

/// The bridge's ear. Each event is put on a broadcast channel and the call
/// returns at once, because the bridge calls this on its submit path.
pub struct BroadcastSink {
    sender: broadcast::Sender<PoolEvent>,
}

impl BroadcastSink {
    pub fn new() -> Self {
        Self::with_capacity(EVENT_CHANNEL_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        BroadcastSink { sender }
    }

    /// A new listener. It hears every event sent after this call.
    pub fn subscribe(&self) -> broadcast::Receiver<PoolEvent> {
        self.sender.subscribe()
    }
}

impl Default for BroadcastSink {
    fn default() -> Self {
        Self::new()
    }
}

impl PoolEvents for BroadcastSink {
    fn share_accepted(&self, report: ShareReport) {
        // An error only means nobody is listening yet, which is not the bridge's problem.
        let _ = self.sender.send(PoolEvent::Share(report));
    }

    fn block_found(&self, report: BlockReport) {
        let _ = self.sender.send(PoolEvent::Block(report));
    }
}
