use crate::PoolEvent;
use pool_chain::{BlockReport, Chain, ShareReport};
use pool_db::{Db, NewBlock, NewShare};
use pool_domain::{MinerAddress, ShareWeight, WorkerName};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::{broadcast, watch};
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at};
use tracing::{debug, error, warn};

/// If the database keeps refusing a batch, this many shares are kept waiting
/// before the oldest are thrown away so memory cannot grow without end.
const MAX_WAITING_SHARES: usize = 200_000;

/// How many different bad addresses are named in the log before it goes quiet.
const MAX_WARNED_ADDRESSES: usize = 1_000;

#[derive(Debug, Clone)]
pub struct ConsumerSettings {
    /// The network id the pool runs on, such as "testnet-10". Shares from an
    /// address that is not on it are not counted.
    pub network: String,
    /// The share window in DAA steps. Shares older than twice this, measured
    /// back from the newest share, are deleted.
    pub window_daa: u64,
    /// How often waiting shares are written even if there are few of them.
    pub flush_interval: Duration,
    /// Write as soon as this many shares are waiting.
    pub flush_rows: usize,
    /// How often old shares are deleted.
    pub prune_interval: Duration,
}

impl ConsumerSettings {
    /// The plan's defaults: write every 250 ms or 500 shares, prune every minute.
    pub fn new(network: impl Into<String>, window_daa: u64) -> Self {
        ConsumerSettings {
            network: network.into(),
            window_daa,
            flush_interval: Duration::from_millis(250),
            flush_rows: 500,
            prune_interval: Duration::from_secs(60),
        }
    }
}

/// Counters the API reads for its metrics page.
#[derive(Debug, Default)]
pub struct AccountantMetrics {
    pub shares_stored: AtomicU64,
    /// Shares not counted: an address from another network, a difficulty of
    /// nothing, or numbers too large to store.
    pub shares_rejected: AtomicU64,
    pub shares_pruned: AtomicU64,
    pub blocks_stored: AtomicU64,
    /// Events the consumer missed because it fell behind the bridge.
    pub events_lagged: AtomicU64,
    pub flush_errors: AtomicU64,
}

fn add(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Ordering::Relaxed);
}

/// Why a share was not counted.
enum Rejected {
    BadAddress(String),
    NoWeight,
    OutOfRange,
}

#[derive(Default)]
struct Waiting {
    shares: Vec<NewShare>,
    blocks: Vec<NewBlock>,
    /// Each miner seen since the last write, with the newest time they were seen.
    miners: HashMap<MinerAddress, u64>,
    warned: HashSet<String>,
}

fn share_from_report<C: Chain>(network: &str, report: ShareReport) -> Result<NewShare, Rejected> {
    let address = MinerAddress::parse::<C>(network, &report.address).map_err(|e| Rejected::BadAddress(e.to_string()))?;
    let weight = ShareWeight::from_difficulty(report.difficulty);
    if weight == ShareWeight::ZERO {
        return Err(Rejected::NoWeight);
    }
    if report.daa_score > i64::MAX as u64 || report.timestamp_ms > i64::MAX as u64 || weight.units() > i64::MAX as u64 {
        return Err(Rejected::OutOfRange);
    }
    // A hash value that is not a number would be stored as nothing and the
    // database would refuse the whole batch; it still was a real share.
    let hash_value = if report.hash_value.is_finite() { report.hash_value } else { 0.0 };
    Ok(NewShare {
        address,
        worker: WorkerName::new(&report.worker),
        weight,
        hash_value,
        daa_score: report.daa_score,
        ts_ms: report.timestamp_ms,
    })
}

fn block_from_report<C: Chain>(network: &str, report: BlockReport) -> Option<NewBlock> {
    if report.daa_score > i64::MAX as u64 || report.blue_score > i64::MAX as u64 || report.timestamp_ms > i64::MAX as u64 {
        return None;
    }
    // A block is never thrown away over its address: it is real, and its reward
    // arrives at the pool either way. The address is kept as the bridge saw it.
    let address =
        MinerAddress::parse::<C>(network, &report.address).unwrap_or_else(|_| MinerAddress::from_trusted(report.address.clone()));
    Some(NewBlock {
        hash: report.hash,
        address,
        worker: WorkerName::new(&report.worker),
        blue_score: report.blue_score,
        daa_score: report.daa_score,
        nonce: report.nonce,
        ts_ms: report.timestamp_ms,
    })
}

impl Waiting {
    /// Take in one event. Returns `true` if it was a block, which is written at once.
    fn accept<C: Chain>(&mut self, network: &str, event: PoolEvent, metrics: &AccountantMetrics) -> bool {
        match event {
            PoolEvent::Share(report) => {
                match share_from_report::<C>(network, report.clone()) {
                    Ok(share) => {
                        let seen = self.miners.entry(share.address.clone()).or_insert(0);
                        *seen = (*seen).max(share.ts_ms);
                        self.shares.push(share);
                    }
                    Err(rejected) => {
                        add(&metrics.shares_rejected, 1);
                        match rejected {
                            Rejected::BadAddress(why) => {
                                if self.warned.len() < MAX_WARNED_ADDRESSES && self.warned.insert(report.address.clone()) {
                                    warn!("shares from {:?} are not counted: {why}", report.address);
                                }
                            }
                            Rejected::NoWeight => debug!("a share with no difficulty was not counted"),
                            Rejected::OutOfRange => debug!("a share with numbers too large to store was not counted"),
                        }
                    }
                }
                false
            }
            PoolEvent::Block(report) => {
                match block_from_report::<C>(network, report) {
                    Some(block) => self.blocks.push(block),
                    None => warn!("a block report with numbers too large to store was not recorded"),
                }
                true
            }
        }
    }

    /// Write everything waiting. Whatever the database refuses stays waiting
    /// for the next try.
    async fn flush(&mut self, db: &Db, metrics: &AccountantMetrics) {
        if !self.shares.is_empty() {
            match db.insert_shares(&self.shares).await {
                Ok(()) => {
                    add(&metrics.shares_stored, self.shares.len() as u64);
                    self.shares.clear();
                }
                Err(error) => {
                    error!("could not write {} waiting shares, will try again: {error}", self.shares.len());
                    add(&metrics.flush_errors, 1);
                    if self.shares.len() > MAX_WAITING_SHARES {
                        let excess = self.shares.len() - MAX_WAITING_SHARES;
                        warn!("{excess} of the oldest waiting shares were thrown away because the database keeps refusing them");
                        self.shares.drain(..excess);
                    }
                    return;
                }
            }
        }
        let mut kept_blocks = Vec::new();
        for block in std::mem::take(&mut self.blocks) {
            match db.insert_block(&block).await {
                Ok(true) => add(&metrics.blocks_stored, 1),
                Ok(false) => debug!("block {} was already recorded", block.hash),
                Err(error) => {
                    error!("could not record block {}, will try again: {error}", block.hash);
                    add(&metrics.flush_errors, 1);
                    kept_blocks.push(block);
                }
            }
        }
        self.blocks = kept_blocks;
        for (address, seen) in std::mem::take(&mut self.miners) {
            if let Err(error) = db.upsert_miner(&address, seen).await {
                error!("could not note that {address} was seen, will try again: {error}");
                add(&metrics.flush_errors, 1);
                self.miners.insert(address, seen);
            }
        }
    }
}

/// Delete shares older than twice the window, measured back from the newest.
async fn prune(db: &Db, window_daa: u64, metrics: &AccountantMetrics) {
    let latest = match db.latest_share_daa().await {
        Ok(Some(latest)) => latest,
        Ok(None) => return,
        Err(error) => {
            error!("could not find the newest share to prune from: {error}");
            add(&metrics.flush_errors, 1);
            return;
        }
    };
    let keep_from = latest.saturating_sub(window_daa.saturating_mul(2));
    match db.prune_shares_below(keep_from).await {
        Ok(0) => {}
        Ok(n) => {
            add(&metrics.shares_pruned, n);
            debug!("pruned {n} shares below DAA score {keep_from}");
        }
        Err(error) => {
            error!("could not prune old shares: {error}");
            add(&metrics.flush_errors, 1);
        }
    }
}

/// Listen to the bridge's events and keep the database up to date: shares and
/// blocks are written in batches, every miner seen is noted, and old shares are
/// pruned. Runs until `shutdown` changes or the sink is dropped, then writes
/// whatever is still queued before returning, so nothing the bridge already
/// reported is lost on a clean stop.
pub async fn run_consumer<C: Chain>(
    db: Db,
    mut events: broadcast::Receiver<PoolEvent>,
    settings: ConsumerSettings,
    metrics: Arc<AccountantMetrics>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut waiting = Waiting::default();
    // The first write is one full period away: an interval's own first tick is
    // immediate, which would write a handful of shares the moment they arrive.
    let mut flush_tick = interval_at(Instant::now() + settings.flush_interval, settings.flush_interval);
    flush_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut prune_tick = interval(settings.prune_interval);
    prune_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            received = events.recv() => match received {
                Ok(event) => {
                    let was_block = waiting.accept::<C>(&settings.network, event, &metrics);
                    if was_block || waiting.shares.len() >= settings.flush_rows {
                        waiting.flush(&db, &metrics).await;
                    }
                }
                Err(RecvError::Lagged(missed)) => {
                    warn!("the pool fell behind the bridge and missed {missed} events");
                    add(&metrics.events_lagged, missed);
                }
                Err(RecvError::Closed) => break,
            },
            _ = flush_tick.tick() => waiting.flush(&db, &metrics).await,
            _ = prune_tick.tick() => prune(&db, settings.window_daa, &metrics).await,
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }

    // Take in what is already queued, then write it.
    loop {
        match events.try_recv() {
            Ok(event) => {
                waiting.accept::<C>(&settings.network, event, &metrics);
            }
            Err(TryRecvError::Lagged(missed)) => {
                warn!("the pool fell behind the bridge and missed {missed} events");
                add(&metrics.events_lagged, missed);
            }
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
    waiting.flush(&db, &metrics).await;
    prune(&db, settings.window_daa, &metrics).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BroadcastSink;
    use crate::test_chain::FakeChain;
    use pool_chain::PoolEvents;
    use std::future::Future;
    use tokio::task::JoinHandle;

    fn share(address: &str, difficulty: f64, daa_score: u64, timestamp_ms: u64) -> ShareReport {
        ShareReport {
            address: address.into(),
            worker: "rig".into(),
            difficulty,
            hash_value: difficulty * 100.0,
            daa_score,
            blue_score: daa_score,
            timestamp_ms,
        }
    }

    fn quick(window_daa: u64) -> ConsumerSettings {
        ConsumerSettings {
            network: "fakenet".into(),
            window_daa,
            flush_interval: Duration::from_millis(20),
            flush_rows: 500,
            prune_interval: Duration::from_secs(3600),
        }
    }

    struct Running {
        db: Db,
        metrics: Arc<AccountantMetrics>,
        stop: watch::Sender<bool>,
        task: JoinHandle<()>,
    }

    impl Running {
        async fn stop(self) -> (Db, Arc<AccountantMetrics>) {
            self.stop.send(true).unwrap();
            self.task.await.unwrap();
            (self.db, self.metrics)
        }
    }

    async fn start(events: broadcast::Receiver<PoolEvent>, settings: ConsumerSettings) -> Running {
        let db = Db::open_in_memory().await.unwrap();
        let metrics = Arc::new(AccountantMetrics::default());
        let (stop, shutdown) = watch::channel(false);
        let task = tokio::spawn(run_consumer::<FakeChain>(db.clone(), events, settings, metrics.clone(), shutdown));
        Running { db, metrics, stop, task }
    }

    async fn wait_until<F, Fut>(what: &str, mut check: F)
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = bool>,
    {
        for _ in 0..500 {
            if check().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("gave up waiting for {what}");
    }

    #[tokio::test]
    async fn ten_thousand_shares_arrive_with_the_right_counts_and_weights_per_miner() {
        let sink = BroadcastSink::with_capacity(16_384);
        let running = start(sink.subscribe(), quick(1_000_000)).await;

        let mut expected_weight = [0u64; 5];
        let mut newest = [0u64; 5];
        for i in 0..10_000u64 {
            let miner = (i % 5) as usize;
            let difficulty = (1 + i % 7) as f64;
            expected_weight[miner] += ShareWeight::from_difficulty(difficulty).units();
            newest[miner] = 1_000 + i;
            sink.share_accepted(share(&format!("fake:m{miner}"), difficulty, 1 + i / 10, 1_000 + i));
        }
        let (db, metrics) = running.stop().await;

        assert_eq!(db.count_shares().await.unwrap(), 10_000);
        assert_eq!(metrics.shares_stored.load(Ordering::Relaxed), 10_000);
        let weights = db.weights_in_window(0, 1_000).await.unwrap();
        assert_eq!(weights.len(), 5);
        for (miner, (address, weight)) in weights.iter().enumerate() {
            assert_eq!(address.as_str(), format!("fake:m{miner}"));
            assert_eq!(weight.units(), expected_weight[miner], "miner {miner}");
        }
        assert_eq!(db.count_miners().await.unwrap(), 5);
        for (miner, newest_seen) in newest.iter().enumerate() {
            let address = MinerAddress::from_trusted(format!("fake:m{miner}"));
            assert_eq!(db.miner_times(&address).await.unwrap().unwrap().1, *newest_seen, "last seen for miner {miner}");
        }
    }

    #[tokio::test]
    async fn a_block_becomes_a_row_and_a_repeat_is_not_counted_twice() {
        let sink = BroadcastSink::new();
        let running = start(sink.subscribe(), quick(1_000)).await;
        let block = BlockReport {
            address: "fake:m1".into(),
            worker: "asic1".into(),
            hash: "abc123".into(),
            blue_score: 40,
            daa_score: 41,
            nonce: u64::MAX,
            timestamp_ms: 99,
        };
        sink.block_found(block.clone());
        sink.block_found(block);
        let db = running.db.clone();
        wait_until("the block row", || async { db.get_block("abc123").await.unwrap().is_some() }).await;

        let (db, metrics) = running.stop().await;
        let row = db.get_block("abc123").await.unwrap().unwrap();
        assert_eq!(
            (row.address.as_str(), row.worker.as_str(), row.blue_score, row.daa_score, row.nonce, row.ts_ms),
            ("fake:m1", "asic1", 40, 41, u64::MAX, 99)
        );
        assert_eq!(metrics.blocks_stored.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_block_from_an_odd_address_is_still_recorded() {
        let sink = BroadcastSink::new();
        let running = start(sink.subscribe(), quick(1_000)).await;
        sink.block_found(BlockReport {
            address: "somewhere else".into(),
            worker: "w".into(),
            hash: "ff".into(),
            blue_score: 1,
            daa_score: 1,
            nonce: 1,
            timestamp_ms: 1,
        });
        let (db, _) = running.stop().await;
        assert_eq!(db.get_block("ff").await.unwrap().unwrap().address.as_str(), "somewhere else");
    }

    #[tokio::test]
    async fn shares_from_the_wrong_network_or_with_no_difficulty_are_not_counted() {
        let sink = BroadcastSink::new();
        let running = start(sink.subscribe(), quick(1_000)).await;
        sink.share_accepted(share("kaspa:qq", 4.0, 10, 1));
        sink.share_accepted(share("fake:good", 4.0, 10, 2));
        sink.share_accepted(share("fake:nodiff", 0.0, 10, 3));
        sink.share_accepted(share("fake:nan", f64::NAN, 10, 4));
        sink.share_accepted(share("fake:huge", 4.0, u64::MAX, 5));
        let (db, metrics) = running.stop().await;

        assert_eq!(db.count_shares().await.unwrap(), 1);
        assert_eq!(db.count_miners().await.unwrap(), 1);
        assert_eq!(metrics.shares_rejected.load(Ordering::Relaxed), 4);
        assert_eq!(metrics.shares_stored.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_share_whose_hash_value_is_not_a_number_is_still_counted() {
        let sink = BroadcastSink::new();
        let running = start(sink.subscribe(), quick(1_000)).await;
        let mut odd = share("fake:m1", 4.0, 10, 1);
        odd.hash_value = f64::NAN;
        sink.share_accepted(odd);
        sink.share_accepted(share("fake:m1", 4.0, 11, 2));
        let (db, _) = running.stop().await;
        assert_eq!(db.count_shares().await.unwrap(), 2, "one odd value must not cost the whole batch");
    }

    #[tokio::test]
    async fn waiting_shares_are_written_on_the_timer_even_when_there_are_few() {
        let sink = BroadcastSink::new();
        let running = start(sink.subscribe(), quick(1_000)).await;
        for i in 0..3 {
            sink.share_accepted(share("fake:m1", 1.0, 5, i + 1));
        }
        let db = running.db.clone();
        wait_until("the timer write", || async { db.count_shares().await.unwrap() == 3 }).await;
        running.stop().await;
    }

    #[tokio::test]
    async fn waiting_shares_are_written_as_soon_as_enough_pile_up() {
        let sink = BroadcastSink::new();
        let mut settings = quick(1_000);
        settings.flush_interval = Duration::from_secs(3600);
        settings.flush_rows = 100;
        let running = start(sink.subscribe(), settings).await;
        for i in 0..250 {
            sink.share_accepted(share("fake:m1", 1.0, 5, i + 1));
        }
        let db = running.db.clone();
        wait_until("two full batches", || async { db.count_shares().await.unwrap() >= 200 }).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(running.db.count_shares().await.unwrap(), 200, "the last fifty wait for a batch or the timer");

        let (db, _) = running.stop().await;
        assert_eq!(db.count_shares().await.unwrap(), 250, "stopping writes what was waiting");
    }

    #[tokio::test]
    async fn shares_older_than_twice_the_window_are_pruned_and_the_boundary_is_exact() {
        let sink = BroadcastSink::with_capacity(4_096);
        let mut settings = quick(100);
        settings.prune_interval = Duration::from_millis(20);
        let running = start(sink.subscribe(), settings).await;
        for daa in 1..=1_000u64 {
            sink.share_accepted(share("fake:m1", 1.0, daa, daa));
        }
        // The newest is 1000, twice the window is 200, so DAA 800 and up stay: 201 shares.
        let db = running.db.clone();
        wait_until("the prune", || async { db.count_shares().await.unwrap() == 201 }).await;
        let (db, metrics) = running.stop().await;
        assert!(db.weights_in_window(0, 799).await.unwrap().is_empty(), "799 and below are gone");
        assert_eq!(db.weights_in_window(799, 800).await.unwrap().len(), 1, "800 itself is kept");
        assert_eq!(metrics.shares_pruned.load(Ordering::Relaxed), 799);
    }

    #[tokio::test]
    async fn a_consumer_that_falls_behind_says_how_many_events_it_missed_and_carries_on() {
        let sink = BroadcastSink::with_capacity(16);
        let events = sink.subscribe();
        for i in 0..100 {
            sink.share_accepted(share("fake:m1", 1.0, 5, i + 1));
        }
        let running = start(events, quick(1_000)).await;
        let (db, metrics) = running.stop().await;
        assert_eq!(metrics.events_lagged.load(Ordering::Relaxed), 84);
        assert_eq!(db.count_shares().await.unwrap(), 16, "the sixteen newest were still heard");
    }

    #[tokio::test]
    async fn it_stops_by_itself_when_the_sink_is_dropped() {
        let sink = BroadcastSink::new();
        let running = start(sink.subscribe(), quick(1_000)).await;
        sink.share_accepted(share("fake:m1", 1.0, 5, 1));
        drop(sink);
        running.task.await.unwrap();
        assert_eq!(running.db.count_shares().await.unwrap(), 1);
    }
}
