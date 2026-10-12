use crate::AccountantMetrics;
use crate::rewards::{is_mature, now_ms};
use pool_chain::{NodeError, NodeView, SharedNode};
use pool_db::{Db, DbError};
use pool_domain::{Account, Amount, allocate};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, error, info, warn};

/// The plan's default share window: one hour at ten blocks a second.
pub const DEFAULT_WINDOW_DAA: u64 = 36_000;

/// The plan's default fee: one percent, in basis points.
pub const DEFAULT_FEE_BP: u16 = 100;

#[derive(Debug, Clone)]
pub struct AllocatorSettings {
    /// A reward is split among the shares in the `window_daa` steps up to and
    /// including the DAA score it was created at.
    pub window_daa: u64,
    /// The pool's fee in basis points (hundredths of a percent).
    pub fee_bp: u16,
    /// How many steps a reward waits before it can be split (the chain's coinbase maturity).
    pub maturity_daa: u64,
    /// How often rewards are checked for.
    pub interval: Duration,
}

impl AllocatorSettings {
    pub fn new(maturity_daa: u64) -> Self {
        AllocatorSettings { window_daa: DEFAULT_WINDOW_DAA, fee_bp: DEFAULT_FEE_BP, maturity_daa, interval: Duration::from_secs(30) }
    }
}

/// What one pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocationSummary {
    /// Rewards split in this pass.
    pub rewards_split: u64,
    /// The sum of those rewards.
    pub total: Amount,
    /// What the pool kept of them.
    pub fee: Amount,
    /// Rewards still waiting to mature.
    pub still_waiting: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AllocationError {
    #[error("{0}")]
    Node(#[from] NodeError),
    #[error("{0}")]
    Db(#[from] DbError),
}

/// Split every reward that has matured and not yet been split. Each one is
/// divided among the shares in its window and written in one transaction, so a
/// failure part-way leaves every reward either fully split or untouched, and a
/// reward already split is never split again.
///
/// The shares must still be on hand: the consumer prunes shares older than
/// twice the window, so rewards must be split well within one window of
/// maturing. They are checked for every few seconds, so this only matters if
/// the pool has been stopped for hours.
pub async fn allocate_matured(
    db: &Db,
    node: &dyn NodeView,
    settings: &AllocatorSettings,
    now_ms: u64,
) -> Result<AllocationSummary, AllocationError> {
    let tip = node.virtual_daa_score().await?;
    let mut summary = AllocationSummary::default();

    for reward in db.unallocated_rewards().await? {
        if !is_mature(reward.block_daa_score, tip, settings.maturity_daa) {
            summary.still_waiting += 1;
            continue;
        }
        let from = reward.block_daa_score.saturating_sub(settings.window_daa);
        let weights = db.weights_in_window(from, reward.block_daa_score).await?;
        let split = allocate(reward.amount, settings.fee_bp, &weights);

        let mut entries = vec![(Account::Pool, split.fee)];
        entries.extend(split.per_miner.iter().map(|(address, amount)| (Account::Miner(address.clone()), *amount)));

        match db.apply_allocation(&reward.outpoint, &entries, now_ms).await {
            Ok(()) => {
                summary.rewards_split += 1;
                summary.total = summary.total.checked_add(reward.amount).unwrap_or(Amount::new(u64::MAX));
                summary.fee = summary.fee.checked_add(split.fee).unwrap_or(Amount::new(u64::MAX));
                debug!("split reward {} ({} units) among {} miner(s)", reward.outpoint, reward.amount.units(), split.per_miner.len());
            }
            // Another pass got there first. Nothing to do.
            Err(DbError::AlreadyAllocated(outpoint)) => debug!("reward {outpoint} was already split"),
            Err(other) => return Err(other.into()),
        }
    }
    Ok(summary)
}

/// Check for matured rewards every `settings.interval` until `shutdown` changes.
pub async fn run_allocator(
    db: Db,
    node: SharedNode,
    settings: AllocatorSettings,
    metrics: Arc<AccountantMetrics>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tick = interval(settings.interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut failing = false;
    loop {
        tokio::select! {
            _ = tick.tick() => match allocate_matured(&db, node.as_ref(), &settings, now_ms()).await {
                Ok(summary) => {
                    failing = false;
                    if summary.rewards_split > 0 {
                        metrics.rewards_allocated.fetch_add(summary.rewards_split, Ordering::Relaxed);
                        info!("split {} reward(s) totalling {} units, the pool kept {}", summary.rewards_split, summary.total.units(), summary.fee.units());
                    }
                }
                Err(AllocationError::Node(why)) => {
                    metrics.node_errors.fetch_add(1, Ordering::Relaxed);
                    if !failing {
                        warn!("could not check which rewards are ready, will keep trying: {why}");
                        failing = true;
                    }
                }
                Err(AllocationError::Db(why)) => {
                    metrics.flush_errors.fetch_add(1, Ordering::Relaxed);
                    error!("could not split rewards: {why}");
                }
            },
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
    use pool_db::NewShare;
    use pool_domain::{MinerAddress, ShareWeight, WorkerName};

    fn who(name: &str) -> MinerAddress {
        MinerAddress::from_trusted(format!("fake:{name}"))
    }

    async fn add_share(db: &Db, name: &str, daa_score: u64, weight: u64) {
        db.insert_share(&NewShare {
            address: who(name),
            worker: WorkerName::new("rig"),
            weight: ShareWeight::new(weight),
            hash_value: 1.0,
            daa_score,
            ts_ms: 1,
        })
        .await
        .unwrap();
    }

    fn settings(window_daa: u64, fee_bp: u16) -> AllocatorSettings {
        AllocatorSettings { window_daa, fee_bp, maturity_daa: 1_000, interval: Duration::from_millis(10) }
    }

    async fn balance(db: &Db, account: Account) -> (u64, u64) {
        db.balance_of(&account).await.unwrap().map(|b| (b.balance.units(), b.total_earned.units())).unwrap_or((0, 0))
    }

    #[tokio::test]
    async fn two_rewards_with_overlapping_windows_come_out_as_worked_by_hand() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_daa(1_300);
        // Window 100, fee 10%. Reward 1 is at DAA 200, so its window is (100, 200].
        // Reward 2 is at 260, so (160, 260]. Reward 3 is at 500 and not yet mature (needs 1500).
        add_share(&db, "a", 150, 100).await; // reward 1 only
        add_share(&db, "b", 170, 300).await; // both
        add_share(&db, "c", 250, 100).await; // reward 2 only
        add_share(&db, "z", 90, 9_999).await; // before both windows
        db.record_reward_if_new("r1:0", Amount::new(10_000), 200, 0).await.unwrap();
        db.record_reward_if_new("r2:0", Amount::new(4_000), 260, 0).await.unwrap();
        db.record_reward_if_new("r3:0", Amount::new(7_000), 500, 0).await.unwrap();

        let summary = allocate_matured(&db, &node, &settings(100, 1_000), 77).await.unwrap();
        assert_eq!(
            summary,
            AllocationSummary { rewards_split: 2, total: Amount::new(14_000), fee: Amount::new(1_400), still_waiting: 1 }
        );

        // Reward 1: fee 1000, net 9000 split a:b = 100:300 -> a 2250, b 6750.
        // Reward 2: fee 400, net 3600 split b:c = 300:100 -> b 2700, c 900.
        assert_eq!(balance(&db, Account::Miner(who("a"))).await, (2_250, 2_250));
        assert_eq!(balance(&db, Account::Miner(who("b"))).await, (9_450, 9_450));
        assert_eq!(balance(&db, Account::Miner(who("c"))).await, (900, 900));
        assert_eq!(balance(&db, Account::Pool).await, (1_400, 1_400));
        assert_eq!(balance(&db, Account::Miner(who("z"))).await, (0, 0), "a share before the window earns nothing");

        let waiting = db.unallocated_rewards().await.unwrap();
        assert_eq!(waiting.iter().map(|r| r.outpoint.as_str()).collect::<Vec<_>>(), ["r3:0"], "the immature one is untouched");

        // Money in equals money out.
        let all = [
            balance(&db, Account::Miner(who("a"))).await.0,
            balance(&db, Account::Miner(who("b"))).await.0,
            balance(&db, Account::Miner(who("c"))).await.0,
            balance(&db, Account::Pool).await.0,
        ];
        assert_eq!(all.iter().sum::<u64>(), 14_000);
    }

    #[tokio::test]
    async fn the_window_includes_its_end_and_excludes_its_start() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_daa(5_000);
        // Reward at 200, window 100: the window is (100, 200].
        add_share(&db, "at_start", 100, 1_000).await; // exactly block - window: out
        add_share(&db, "just_in", 101, 1_000).await; // in
        add_share(&db, "at_end", 200, 1_000).await; // exactly the block: in
        add_share(&db, "after", 201, 1_000).await; // out
        db.record_reward_if_new("r:0", Amount::new(1_000), 200, 0).await.unwrap();

        allocate_matured(&db, &node, &settings(100, 0), 1).await.unwrap();
        assert_eq!(balance(&db, Account::Miner(who("at_start"))).await.0, 0);
        assert_eq!(balance(&db, Account::Miner(who("after"))).await.0, 0);
        assert_eq!(balance(&db, Account::Miner(who("just_in"))).await.0, 500);
        assert_eq!(balance(&db, Account::Miner(who("at_end"))).await.0, 500);
    }

    #[tokio::test]
    async fn a_reward_is_matured_at_exactly_the_limit_and_not_a_step_before() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        add_share(&db, "a", 150, 1).await;
        db.record_reward_if_new("r:0", Amount::new(1_000), 200, 0).await.unwrap();

        node.set_daa(1_199);
        assert_eq!(allocate_matured(&db, &node, &settings(100, 0), 1).await.unwrap().rewards_split, 0);
        node.set_daa(1_200);
        assert_eq!(allocate_matured(&db, &node, &settings(100, 0), 1).await.unwrap().rewards_split, 1);
    }

    #[tokio::test]
    async fn running_again_changes_nothing() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_daa(5_000);
        add_share(&db, "a", 150, 1).await;
        db.record_reward_if_new("r:0", Amount::new(1_000), 200, 0).await.unwrap();

        assert_eq!(allocate_matured(&db, &node, &settings(100, 100), 1).await.unwrap().rewards_split, 1);
        let before = balance(&db, Account::Miner(who("a"))).await;
        let again = allocate_matured(&db, &node, &settings(100, 100), 2).await.unwrap();
        assert_eq!(again, AllocationSummary::default());
        assert_eq!(balance(&db, Account::Miner(who("a"))).await, before, "paid once, not twice");
        assert_eq!(db.get_reward("r:0").await.unwrap().unwrap().allocated_ts_ms, Some(1));
    }

    #[tokio::test]
    async fn a_reward_with_nobody_in_its_window_goes_entirely_to_the_pool() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_daa(5_000);
        db.record_reward_if_new("r:0", Amount::new(1_234), 200, 0).await.unwrap();
        allocate_matured(&db, &node, &settings(100, 100), 1).await.unwrap();
        assert_eq!(balance(&db, Account::Pool).await, (1_234, 1_234));
        assert!(db.unallocated_rewards().await.unwrap().is_empty(), "it is settled, not left waiting");
    }

    #[tokio::test]
    async fn an_early_reward_does_not_look_below_zero() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_daa(5_000);
        add_share(&db, "a", 5, 1).await;
        db.record_reward_if_new("r:0", Amount::new(100), 10, 0).await.unwrap(); // 10 - 36000 would go below zero
        allocate_matured(&db, &node, &AllocatorSettings::new(1_000), 1).await.unwrap();
        assert_eq!(balance(&db, Account::Miner(who("a"))).await.0, 99);
        assert_eq!(balance(&db, Account::Pool).await.0, 1);
    }

    #[tokio::test]
    async fn an_unreachable_node_splits_nothing_and_the_next_pass_works() {
        let db = Db::open_in_memory().await.unwrap();
        let node = FakeNode::default();
        node.set_daa(5_000);
        add_share(&db, "a", 150, 1).await;
        db.record_reward_if_new("r:0", Amount::new(1_000), 200, 0).await.unwrap();
        node.fail_next(1);
        assert!(matches!(allocate_matured(&db, &node, &settings(100, 0), 1).await, Err(AllocationError::Node(_))));
        assert_eq!(db.unallocated_rewards().await.unwrap().len(), 1);
        assert_eq!(allocate_matured(&db, &node, &settings(100, 0), 2).await.unwrap().rewards_split, 1);
    }

    #[tokio::test]
    async fn the_loop_splits_rewards_as_they_mature_and_stops_on_request() {
        let db = Db::open_in_memory().await.unwrap();
        let node = Arc::new(FakeNode::default());
        node.set_daa(5_000);
        add_share(&db, "a", 150, 1).await;
        db.record_reward_if_new("r:0", Amount::new(1_000), 200, 0).await.unwrap();
        let metrics = Arc::new(AccountantMetrics::default());
        let (stop, shutdown) = watch::channel(false);
        let shared: SharedNode = node.clone();
        let task = tokio::spawn(run_allocator(db.clone(), shared, settings(100, 0), metrics.clone(), shutdown));

        for _ in 0..500 {
            if db.unallocated_rewards().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.send(true).unwrap();
        task.await.unwrap();
        assert_eq!(balance(&db, Account::Miner(who("a"))).await.0, 1_000);
        assert_eq!(metrics.rewards_allocated.load(Ordering::Relaxed), 1);
    }
}
