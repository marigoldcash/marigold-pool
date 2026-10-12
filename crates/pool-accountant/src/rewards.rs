use pool_chain::{NodeError, NodeView, SharedNode};
use pool_db::{Db, DbError};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use crate::AccountantMetrics;

/// Whether a reward created at `block_daa_score` can be spent once the node's
/// tip is at `virtual_daa_score`: it has waited `maturity_daa` steps or more.
/// Exactly at the limit it is mature, one step short it is not.
pub fn is_mature(block_daa_score: u64, virtual_daa_score: u64, maturity_daa: u64) -> bool {
    virtual_daa_score.saturating_sub(block_daa_score) >= maturity_daa
}

#[derive(Debug, Clone)]
pub struct WatcherSettings {
    /// The address block rewards are paid to.
    pub pool_address: String,
    /// How often the node is asked.
    pub poll_interval: Duration,
    /// How long after a block is found the pool keeps asking what colour it is.
    pub colour_window: Duration,
}

impl WatcherSettings {
    /// The plan's defaults: ask every ten seconds, and keep asking about a
    /// block's colour for a minute.
    pub fn new(pool_address: impl Into<String>) -> Self {
        WatcherSettings {
            pool_address: pool_address.into(),
            poll_interval: Duration::from_secs(10),
            colour_window: Duration::from_secs(60),
        }
    }
}

/// What one look at the node found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PollSummary {
    /// Rewards seen for the first time.
    pub new_rewards: u64,
    /// Rewards at the address that were already known.
    pub known_rewards: u64,
    /// Blocks whose colour was learned.
    pub blocks_coloured: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum PollError {
    #[error("{0}")]
    Node(#[from] NodeError),
    #[error("{0}")]
    Db(#[from] DbError),
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Look at the node once. Every block reward (coinbase output) at the pool's
/// address is remembered the first time it is seen, even while it is still
/// immature, so spending it later cannot make it disappear from the books.
/// Blocks found in the last `colour_window` that have no colour yet are asked
/// about. Nothing is written if the node cannot be reached for its outputs.
pub async fn poll_once(db: &Db, node: &dyn NodeView, settings: &WatcherSettings, now_ms: u64) -> Result<PollSummary, PollError> {
    let mut summary = PollSummary::default();

    let utxos = node.utxos_at(&settings.pool_address).await?;
    for utxo in utxos.iter().filter(|u| u.is_coinbase) {
        if utxo.amount == 0 {
            continue;
        }
        let is_new =
            db.record_reward_if_new(&utxo.outpoint, pool_domain::Amount::new(utxo.amount), utxo.block_daa_score, now_ms).await?;
        if is_new {
            summary.new_rewards += 1;
        } else {
            summary.known_rewards += 1;
        }
    }

    let since = now_ms.saturating_sub(settings.colour_window.as_millis() as u64);
    for block in db.unresolved_blocks_since(since).await? {
        match node.block_is_blue(&block.hash).await {
            Ok(blue) => {
                if db.set_block_blue(&block.hash, blue).await? {
                    summary.blocks_coloured += 1;
                }
            }
            // Not known yet, or the node went away: ask again next time.
            Err(error) => debug!("colour of block {} is not known yet: {error}", block.hash),
        }
    }
    Ok(summary)
}

/// Keep looking at the node until `shutdown` changes. A look that fails is
/// logged and counted and the next one tries again, so a node restart costs a
/// few polls and nothing else.
pub async fn run_reward_watcher(
    db: Db,
    node: SharedNode,
    settings: WatcherSettings,
    metrics: Arc<AccountantMetrics>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tick = interval(settings.poll_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut failing = false;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                match poll_once(&db, node.as_ref(), &settings, now_ms()).await {
                    Ok(summary) => {
                        if failing {
                            info!("the node is answering again");
                            failing = false;
                        }
                        metrics.rewards_seen.fetch_add(summary.new_rewards, Ordering::Relaxed);
                        metrics.blocks_coloured.fetch_add(summary.blocks_coloured, Ordering::Relaxed);
                        if summary.new_rewards > 0 {
                            info!("{} new block reward(s) arrived at the pool's address", summary.new_rewards);
                        }
                    }
                    Err(error) => {
                        metrics.node_errors.fetch_add(1, Ordering::Relaxed);
                        // Say it once when it starts, not every ten seconds.
                        if !failing {
                            warn!("could not look for rewards, will keep trying: {error}");
                            failing = true;
                        }
                    }
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_node::FakeNode;
    use pool_chain::NodeUtxo;
    use pool_db::NewBlock;
    use pool_domain::{MinerAddress, WorkerName};

    const POOL: &str = "fake:pool";

    fn utxo(outpoint: &str, amount: u64, block_daa_score: u64, is_coinbase: bool) -> NodeUtxo {
        NodeUtxo { outpoint: outpoint.into(), amount, block_daa_score, is_coinbase }
    }

    fn settings() -> WatcherSettings {
        WatcherSettings::new(POOL)
    }

    #[test]
    fn maturity_flips_at_exactly_the_limit() {
        assert!(!is_mature(500, 1499, 1000), "one step short");
        assert!(is_mature(500, 1500, 1000), "exactly at the limit");
        assert!(is_mature(500, 1501, 1000));
        assert!(!is_mature(500, 500, 1000));
        assert!(!is_mature(500, 100, 1000), "a tip behind the block is never mature");
        assert!(is_mature(0, 1000, 1000));
    }

    #[tokio::test]
    async fn only_block_rewards_are_recorded_not_ordinary_payments() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_utxos(vec![utxo("a:0", 500, 100, true), utxo("b:1", 700, 100, false), utxo("c:0", 0, 100, true)]);
        let summary = poll_once(&db, &node, &settings(), 10).await.unwrap();
        assert_eq!(summary, PollSummary { new_rewards: 1, known_rewards: 0, blocks_coloured: 0 });
        let rewards = db.unallocated_rewards().await.unwrap();
        assert_eq!(rewards.len(), 1);
        assert_eq!(
            (rewards[0].outpoint.as_str(), rewards[0].amount.units(), rewards[0].block_daa_score, rewards[0].seen_ts_ms),
            ("a:0", 500, 100, 10)
        );
    }

    #[tokio::test]
    async fn a_reward_seen_before_it_matures_stays_on_the_books_after_it_is_spent() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_daa(150);
        node.set_utxos(vec![utxo("a:0", 500, 100, true)]);
        poll_once(&db, &node, &settings(), 1).await.unwrap();

        node.set_utxos(vec![]); // spent, or simply gone from the address
        let summary = poll_once(&db, &node, &settings(), 2).await.unwrap();
        assert_eq!(summary, PollSummary::default());
        let rewards = db.unallocated_rewards().await.unwrap();
        assert_eq!(rewards.len(), 1, "the reward is still there to be split");
        assert_eq!(rewards[0].outpoint, "a:0");
    }

    #[tokio::test]
    async fn a_reward_is_recorded_once_however_many_times_it_is_seen() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_utxos(vec![utxo("a:0", 500, 100, true)]);
        assert_eq!(poll_once(&db, &node, &settings(), 1).await.unwrap().new_rewards, 1);
        let again = poll_once(&db, &node, &settings(), 2).await.unwrap();
        assert_eq!((again.new_rewards, again.known_rewards), (0, 1));
        assert_eq!(db.get_reward("a:0").await.unwrap().unwrap().seen_ts_ms, 1, "the first sighting is kept");
    }

    #[tokio::test]
    async fn rewards_are_not_recorded_again_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.db");
        let node = FakeNode::default();
        node.set_utxos(vec![utxo("a:0", 500, 100, true), utxo("a:1", 600, 120, true)]);

        let first = Db::open(&path).await.unwrap();
        assert_eq!(poll_once(&first, &node, &settings(), 1).await.unwrap().new_rewards, 2);
        first.close().await;

        let second = Db::open(&path).await.unwrap(); // a fresh start, nothing remembered but the file
        let summary = poll_once(&second, &node, &settings(), 99).await.unwrap();
        assert_eq!((summary.new_rewards, summary.known_rewards), (0, 2));
        assert_eq!(second.unallocated_rewards().await.unwrap().len(), 2);
        second.close().await;
    }

    #[tokio::test]
    async fn a_node_that_cannot_be_reached_changes_nothing_and_says_why() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_utxos(vec![utxo("a:0", 500, 100, true)]);
        node.fail_next(1);
        let error = poll_once(&db, &node, &settings(), 1).await.unwrap_err();
        assert!(matches!(error, PollError::Node(_)), "{error}");
        assert!(db.unallocated_rewards().await.unwrap().is_empty());
        assert_eq!(poll_once(&db, &node, &settings(), 2).await.unwrap().new_rewards, 1, "the next look works");
    }

    fn block(hash: &str, ts_ms: u64) -> NewBlock {
        NewBlock {
            hash: hash.into(),
            address: MinerAddress::from_trusted("fake:m1".into()),
            worker: WorkerName::new("w"),
            blue_score: 1,
            daa_score: 1,
            nonce: 1,
            ts_ms,
        }
    }

    #[tokio::test]
    async fn the_colour_of_a_recent_block_is_filled_in_and_a_stale_one_is_left_alone() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        let now = 1_000_000u64;
        db.insert_block(&block("fresh-blue", now - 10_000)).await.unwrap();
        db.insert_block(&block("fresh-red", now - 20_000)).await.unwrap();
        db.insert_block(&block("not-known-yet", now - 5_000)).await.unwrap();
        db.insert_block(&block("too-old", now - 61_000)).await.unwrap();
        node.set_colour("fresh-blue", true);
        node.set_colour("fresh-red", false);
        node.set_colour("too-old", true);

        let summary = poll_once(&db, &node, &settings(), now).await.unwrap();
        assert_eq!(summary.blocks_coloured, 2);
        assert_eq!(db.get_block("fresh-blue").await.unwrap().unwrap().blue, Some(true));
        assert_eq!(db.get_block("fresh-red").await.unwrap().unwrap().blue, Some(false));
        assert_eq!(db.get_block("not-known-yet").await.unwrap().unwrap().blue, None, "unknown stays unknown");
        assert_eq!(db.get_block("too-old").await.unwrap().unwrap().blue, None, "after the minute nobody asks");

        node.set_colour("not-known-yet", true);
        assert_eq!(poll_once(&db, &node, &settings(), now + 1_000).await.unwrap().blocks_coloured, 1, "it is learned on a later look");
        assert_eq!(db.get_block("not-known-yet").await.unwrap().unwrap().blue, Some(true));
    }

    #[tokio::test]
    async fn the_watcher_keeps_trying_through_a_node_outage_and_counts_the_failures() {
        let db = Db::open_in_memory().await.unwrap();
        let node = Arc::new(FakeNode::default());
        node.set_utxos(vec![utxo("a:0", 500, 100, true)]);
        node.fail_next(3);
        let metrics = Arc::new(AccountantMetrics::default());
        let (stop, shutdown) = watch::channel(false);
        let mut s = settings();
        s.poll_interval = Duration::from_millis(10);
        let shared: SharedNode = node.clone();
        let task = tokio::spawn(run_reward_watcher(db.clone(), shared, s, metrics.clone(), shutdown));

        for _ in 0..500 {
            if db.get_reward("a:0").await.unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.send(true).unwrap();
        task.await.unwrap();

        assert!(db.get_reward("a:0").await.unwrap().is_some(), "the reward was found once the node answered");
        assert_eq!(metrics.node_errors.load(Ordering::Relaxed), 3);
        assert_eq!(metrics.rewards_seen.load(Ordering::Relaxed), 1);
    }
}
