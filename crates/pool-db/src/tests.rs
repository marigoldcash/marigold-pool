use crate::*;
use pool_domain::{Account, Amount, MinerAddress, ShareWeight, WorkerName};
use sqlx::Row;

fn addr(n: u8) -> MinerAddress {
    MinerAddress::from_trusted(format!("fake:miner{n}"))
}

fn share(address: &MinerAddress, daa_score: u64, weight: u64) -> NewShare {
    NewShare {
        address: address.clone(),
        worker: WorkerName::new("rig"),
        weight: ShareWeight::new(weight),
        hash_value: weight as f64,
        daa_score,
        ts_ms: 1_000 + daa_score,
    }
}

async fn db() -> Db {
    Db::open_in_memory().await.unwrap()
}

async fn tables(db: &Db) -> Vec<String> {
    sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations' ORDER BY name")
        .fetch_all(db.read_pool())
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>("name"))
        .collect()
}

async fn migrations_applied(db: &Db) -> i64 {
    sqlx::query("SELECT COUNT(*) AS n FROM _sqlx_migrations").fetch_one(db.read_pool()).await.unwrap().get("n")
}

// ---- schema -------------------------------------------------------------

#[tokio::test]
async fn a_fresh_file_gets_the_schema_and_a_second_open_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pool.db");
    assert!(!path.exists());

    let first = Db::open(&path).await.unwrap();
    assert_eq!(tables(&first).await, ["allocations", "balances", "blocks", "meta", "miners", "payouts", "rewards", "shares"]);
    assert_eq!(migrations_applied(&first).await, 2);
    first.meta_set("kept", "yes").await.unwrap();
    first.close().await;

    let second = Db::open(&path).await.unwrap();
    assert_eq!(migrations_applied(&second).await, 2, "the schema is not applied twice");
    assert_eq!(second.meta_get("kept").await.unwrap().as_deref(), Some("yes"), "existing data survives a reopen");
    second.close().await;
}

#[tokio::test]
async fn the_in_memory_database_has_the_same_schema() {
    let db = db().await;
    assert_eq!(tables(&db).await.len(), 8);
}

// ---- shares -------------------------------------------------------------

#[tokio::test]
async fn shares_are_summed_per_miner_over_a_window_that_excludes_its_start_and_includes_its_end() {
    let db = db().await;
    let (a, b) = (addr(1), addr(2));
    db.insert_shares(&[
        share(&a, 100, 10), // exactly at the start: outside
        share(&a, 101, 20),
        share(&b, 150, 5),
        share(&a, 200, 30), // exactly at the end: inside
        share(&b, 201, 99), // past the end: outside
    ])
    .await
    .unwrap();
    let weights = db.weights_in_window(100, 200).await.unwrap();
    assert_eq!(weights, vec![(a.clone(), ShareWeight::new(50)), (b.clone(), ShareWeight::new(5))]);
    assert_eq!(db.weights_in_window(200, 201).await.unwrap(), vec![(b, ShareWeight::new(99))]);
    assert!(db.weights_in_window(300, 400).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_same_shares_always_give_the_same_list_in_address_order() {
    let db = db().await;
    for n in [5u8, 3, 9, 1] {
        db.insert_share(&share(&addr(n), 10, u64::from(n))).await.unwrap();
    }
    let list: Vec<String> = db.weights_in_window(0, 10).await.unwrap().into_iter().map(|(a, _)| a.to_string()).collect();
    assert_eq!(list, ["fake:miner1", "fake:miner3", "fake:miner5", "fake:miner9"]);
}

#[tokio::test]
async fn pruning_removes_only_what_is_below_the_point() {
    let db = db().await;
    let a = addr(1);
    db.insert_shares(&[share(&a, 10, 1), share(&a, 20, 1), share(&a, 30, 1)]).await.unwrap();
    assert_eq!(db.count_shares().await.unwrap(), 3);
    assert_eq!(db.latest_share_daa().await.unwrap(), Some(30));
    assert_eq!(db.prune_shares_below(20).await.unwrap(), 1, "20 itself stays");
    assert_eq!(db.count_shares().await.unwrap(), 2);
    assert_eq!(db.prune_shares_below(0).await.unwrap(), 0);
}

#[tokio::test]
async fn an_empty_database_has_no_latest_share() {
    assert_eq!(db().await.latest_share_daa().await.unwrap(), None);
}

#[tokio::test]
async fn an_empty_batch_is_fine() {
    db().await.insert_shares(&[]).await.unwrap();
}

#[tokio::test]
async fn a_share_too_large_to_store_is_refused_and_nothing_of_its_batch_is_kept() {
    let db = db().await;
    let a = addr(1);
    let mut bad = share(&a, 10, 1);
    bad.daa_score = u64::MAX;
    let error = db.insert_shares(&[share(&a, 5, 1), bad]).await.unwrap_err();
    assert!(matches!(error, DbError::OutOfRange(_)), "{error}");
    assert_eq!(db.count_shares().await.unwrap(), 0);
}

// ---- miners and blocks --------------------------------------------------

#[tokio::test]
async fn a_miner_keeps_the_time_first_seen_and_last_seen_only_moves_forward() {
    let db = db().await;
    let a = addr(1);
    assert_eq!(db.miner_times(&a).await.unwrap(), None);
    db.upsert_miner(&a, 1_000).await.unwrap();
    db.upsert_miner(&a, 5_000).await.unwrap();
    db.upsert_miner(&a, 3_000).await.unwrap();
    assert_eq!(db.miner_times(&a).await.unwrap(), Some((1_000, 5_000)));
    db.upsert_miner(&addr(2), 7).await.unwrap();
    assert_eq!(db.count_miners().await.unwrap(), 2);
}

fn block(hash: &str) -> NewBlock {
    NewBlock {
        hash: hash.into(),
        address: addr(1),
        worker: WorkerName::new("rig"),
        blue_score: 77,
        daa_score: 88,
        nonce: u64::MAX - 5,
        ts_ms: 123,
    }
}

#[tokio::test]
async fn a_block_reported_twice_is_stored_once() {
    let db = db().await;
    assert!(db.insert_block(&block("aa")).await.unwrap());
    assert!(!db.insert_block(&block("aa")).await.unwrap());
    let row = db.get_block("aa").await.unwrap().unwrap();
    assert_eq!(row.nonce, u64::MAX - 5, "a nonce above i64::MAX comes back exactly");
    assert_eq!((row.blue_score, row.daa_score, row.ts_ms), (77, 88, 123));
    assert_eq!(row.blue, None);
    assert_eq!(db.get_block("missing").await.unwrap(), None);
}

#[tokio::test]
async fn a_blocks_colour_can_be_recorded_once_it_is_known() {
    let db = db().await;
    db.insert_block(&block("aa")).await.unwrap();
    assert!(db.set_block_blue("aa", true).await.unwrap());
    assert_eq!(db.get_block("aa").await.unwrap().unwrap().blue, Some(true));
    assert!(db.set_block_blue("aa", false).await.unwrap());
    assert_eq!(db.get_block("aa").await.unwrap().unwrap().blue, Some(false));
    assert!(!db.set_block_blue("nope", true).await.unwrap());
}

#[tokio::test]
async fn blocks_waiting_for_a_colour_can_be_listed_from_a_time_on() {
    let db = db().await;
    for (hash, ts) in [("old", 100u64), ("mid", 200), ("new", 300)] {
        let mut b = block(hash);
        b.ts_ms = ts;
        db.insert_block(&b).await.unwrap();
    }
    db.set_block_blue("mid", true).await.unwrap();
    let hashes: Vec<String> = db.unresolved_blocks_since(150).await.unwrap().into_iter().map(|b| b.hash).collect();
    assert_eq!(hashes, ["new"], "mid is coloured, old is too old");
    let all: Vec<String> = db.unresolved_blocks_since(0).await.unwrap().into_iter().map(|b| b.hash).collect();
    assert_eq!(all, ["old", "new"]);
}

// ---- rewards and allocation ---------------------------------------------

fn amount(n: u64) -> Amount {
    Amount::new(n)
}

#[tokio::test]
async fn a_reward_is_recorded_once_however_often_it_is_seen() {
    let db = db().await;
    assert!(db.record_reward_if_new("tx:0", amount(500), 1_000, 9).await.unwrap());
    assert!(!db.record_reward_if_new("tx:0", amount(999), 2_000, 99).await.unwrap(), "a second sighting changes nothing");
    let reward = db.get_reward("tx:0").await.unwrap().unwrap();
    assert_eq!((reward.amount, reward.block_daa_score, reward.seen_ts_ms, reward.allocated_ts_ms), (amount(500), 1_000, 9, None));
}

#[tokio::test]
async fn unallocated_rewards_come_oldest_block_first() {
    let db = db().await;
    db.record_reward_if_new("tx:b", amount(1), 300, 0).await.unwrap();
    db.record_reward_if_new("tx:a", amount(1), 300, 0).await.unwrap();
    db.record_reward_if_new("tx:c", amount(1), 100, 0).await.unwrap();
    let order: Vec<String> = db.unallocated_rewards().await.unwrap().into_iter().map(|r| r.outpoint).collect();
    assert_eq!(order, ["tx:c", "tx:a", "tx:b"]);
}

async fn split_setup() -> (Db, MinerAddress, MinerAddress) {
    let db = db().await;
    db.record_reward_if_new("tx:0", amount(1_000), 500, 0).await.unwrap();
    (db, addr(1), addr(2))
}

fn entries(a: &MinerAddress, b: &MinerAddress) -> Vec<(Account, Amount)> {
    vec![(Account::Pool, amount(10)), (Account::Miner(a.clone()), amount(600)), (Account::Miner(b.clone()), amount(390))]
}

#[tokio::test]
async fn a_split_credits_every_account_and_marks_the_reward_done() {
    let (db, a, b) = split_setup().await;
    db.apply_allocation("tx:0", &entries(&a, &b), 4_242).await.unwrap();

    let reward = db.get_reward("tx:0").await.unwrap().unwrap();
    assert_eq!(reward.allocated_ts_ms, Some(4_242));
    assert!(db.unallocated_rewards().await.unwrap().is_empty());

    let for_a = db.balance_of(&Account::Miner(a.clone())).await.unwrap().unwrap();
    assert_eq!((for_a.balance, for_a.total_earned, for_a.total_paid), (amount(600), amount(600), Amount::ZERO));
    let pool = db.balance_of(&Account::Pool).await.unwrap().unwrap();
    assert_eq!(pool.balance, amount(10), "the fee is kept under the pool's own key");

    let rows = db.allocations_for_reward("tx:0").await.unwrap();
    assert_eq!(rows.iter().map(|(_, n)| n.units()).sum::<u64>(), 1_000, "the rows add up to the reward");
    assert_eq!(rows.len(), 3);
}

#[tokio::test]
async fn applying_the_same_split_twice_fails_the_second_time_and_changes_nothing() {
    let (db, a, b) = split_setup().await;
    db.apply_allocation("tx:0", &entries(&a, &b), 1).await.unwrap();
    let error = db.apply_allocation("tx:0", &entries(&a, &b), 2).await.unwrap_err();
    assert!(matches!(error, DbError::AlreadyAllocated(_)), "{error}");
    let for_a = db.balance_of(&Account::Miner(a)).await.unwrap().unwrap();
    assert_eq!(for_a.balance, amount(600), "the miner was not credited twice");
    assert_eq!(db.get_reward("tx:0").await.unwrap().unwrap().allocated_ts_ms, Some(1));
}

#[tokio::test]
async fn a_split_that_does_not_add_up_is_refused_whole() {
    let (db, a, b) = split_setup().await;
    let mut wrong = entries(&a, &b);
    wrong[1].1 = amount(601);
    let error = db.apply_allocation("tx:0", &wrong, 1).await.unwrap_err();
    assert!(matches!(error, DbError::AllocationMismatch { expected: 1_000, got: 1_001, .. }), "{error}");
    assert!(db.balance_of(&Account::Miner(a)).await.unwrap().is_none());
    assert_eq!(db.unallocated_rewards().await.unwrap().len(), 1, "the reward is still waiting");
    assert!(db.allocations_for_reward("tx:0").await.unwrap().is_empty());
}

#[tokio::test]
async fn a_split_of_an_unknown_reward_is_refused() {
    let (db, a, b) = split_setup().await;
    let error = db.apply_allocation("tx:9", &entries(&a, &b), 1).await.unwrap_err();
    assert!(matches!(error, DbError::UnknownReward(_)), "{error}");
}

#[tokio::test]
async fn a_split_naming_an_account_twice_is_refused() {
    let (db, a, _) = split_setup().await;
    let twice = vec![(Account::Miner(a.clone()), amount(500)), (Account::Miner(a), amount(500))];
    let error = db.apply_allocation("tx:0", &twice, 1).await.unwrap_err();
    assert!(matches!(error, DbError::DuplicateAccount(_)), "{error}");
}

#[tokio::test]
async fn entries_of_nothing_are_dropped_rather_than_stored() {
    let (db, a, b) = split_setup().await;
    let with_zero = vec![(Account::Pool, amount(0)), (Account::Miner(a), amount(1_000)), (Account::Miner(b.clone()), amount(0))];
    db.apply_allocation("tx:0", &with_zero, 1).await.unwrap();
    assert_eq!(db.allocations_for_reward("tx:0").await.unwrap().len(), 1);
    assert!(db.balance_of(&Account::Miner(b)).await.unwrap().is_none());
    assert!(db.balance_of(&Account::Pool).await.unwrap().is_none());
}

#[tokio::test]
async fn balances_build_up_across_rewards() {
    let db = db().await;
    let a = addr(1);
    for (i, n) in [(0u64, 300u64), (1, 200)] {
        let key = format!("tx:{i}");
        db.record_reward_if_new(&key, amount(n), i, 0).await.unwrap();
        db.apply_allocation(&key, &[(Account::Miner(a.clone()), amount(n))], i).await.unwrap();
    }
    let balance = db.balance_of(&Account::Miner(a)).await.unwrap().unwrap();
    assert_eq!((balance.balance, balance.total_earned), (amount(500), amount(500)));
}

#[tokio::test]
async fn only_miners_at_or_over_the_threshold_are_eligible_and_the_pool_never_is() {
    let db = db().await;
    let (a, b, c) = (addr(1), addr(2), addr(3));
    for (key, who, n) in [("tx:a", &a, 1_000u64), ("tx:b", &b, 999), ("tx:c", &c, 5_000)] {
        db.record_reward_if_new(key, amount(n + 7), 1, 0).await.unwrap();
        db.apply_allocation(key, &[(Account::Pool, amount(7)), (Account::Miner(who.clone()), amount(n))], 1).await.unwrap();
    }
    let eligible = db.balances_eligible(amount(1_000)).await.unwrap();
    assert_eq!(
        eligible.iter().map(|e| (e.address.to_string(), e.balance.units())).collect::<Vec<_>>(),
        [("fake:miner1".to_string(), 1_000), ("fake:miner3".to_string(), 5_000)]
    );
    assert!(db.balances_eligible(Amount::ZERO).await.unwrap().iter().all(|e| e.address.as_str() != "pool"));
    assert_eq!(db.balances_eligible(Amount::ZERO).await.unwrap().len(), 3);
}

// ---- payouts ------------------------------------------------------------

async fn funded(n: u64) -> (Db, MinerAddress) {
    let db = db().await;
    let a = addr(1);
    db.record_reward_if_new("tx:0", amount(n), 1, 0).await.unwrap();
    db.apply_allocation("tx:0", &[(Account::Miner(a.clone()), amount(n))], 1).await.unwrap();
    (db, a)
}

fn payout(key: &str, a: &MinerAddress, n: u64) -> NewPayout {
    NewPayout { idempotency_key: key.into(), address: a.clone(), amount: amount(n) }
}

#[tokio::test]
async fn writing_a_payout_down_takes_it_out_of_the_balance() {
    let (db, a) = funded(1_000).await;
    let written = db.create_pending_payouts(&[payout("k1", &a, 400)], 50).await.unwrap();
    assert_eq!(written.len(), 1);
    assert_eq!((written[0].status, written[0].amount, written[0].created_ts_ms), (PayoutStatus::Pending, amount(400), 50));
    let balance = db.balance_of(&Account::Miner(a)).await.unwrap().unwrap();
    assert_eq!(balance.balance, amount(600));
    assert_eq!(balance.total_earned, amount(1_000), "what was earned does not shrink");
}

#[tokio::test]
async fn a_batch_where_one_payout_cannot_be_covered_writes_none() {
    let (db, a) = funded(1_000).await;
    let error = db.create_pending_payouts(&[payout("k1", &a, 700), payout("k2", &a, 700)], 1).await.unwrap_err();
    assert!(matches!(error, DbError::InsufficientBalance { have: 300, wanted: 700, .. }), "{error}");
    assert_eq!(db.balance_of(&Account::Miner(a)).await.unwrap().unwrap().balance, amount(1_000), "the first one was rolled back too");
    assert!(db.payouts_with_status(PayoutStatus::Pending).await.unwrap().is_empty());
}

#[tokio::test]
async fn someone_owed_nothing_cannot_be_paid() {
    let db = db().await;
    let error = db.create_pending_payouts(&[payout("k", &addr(9), 1)], 1).await.unwrap_err();
    assert!(matches!(error, DbError::InsufficientBalance { have: 0, .. }), "{error}");
}

#[tokio::test]
async fn a_payout_key_can_only_be_used_once() {
    let (db, a) = funded(1_000).await;
    db.create_pending_payouts(&[payout("same", &a, 100)], 1).await.unwrap();
    let error = db.create_pending_payouts(&[payout("same", &a, 100)], 2).await.unwrap_err();
    assert!(matches!(error, DbError::DuplicatePayoutKey(_)), "{error}");
    assert_eq!(db.balance_of(&Account::Miner(a)).await.unwrap().unwrap().balance, amount(900), "only the first was taken");
    assert!(db.payout_by_key("same").await.unwrap().is_some());
    assert!(db.payout_by_key("other").await.unwrap().is_none());
}

#[tokio::test]
async fn a_payout_can_not_be_for_nothing() {
    let (db, a) = funded(1_000).await;
    let error = db.create_pending_payouts(&[payout("k", &a, 0)], 1).await.unwrap_err();
    assert!(matches!(error, DbError::OutOfRange(_)), "{error}");
}

#[tokio::test]
async fn a_payout_goes_pending_then_sent_then_confirmed_and_only_then_counts_as_paid() {
    let (db, a) = funded(1_000).await;
    let id = db.create_pending_payouts(&[payout("k", &a, 400)], 1).await.unwrap()[0].id;

    db.mark_payout_sent(id, "tx123", 2).await.unwrap();
    let sent = db.get_payout(id).await.unwrap().unwrap();
    assert_eq!((sent.status, sent.txid.as_deref(), sent.updated_ts_ms), (PayoutStatus::Sent, Some("tx123"), 2));
    assert_eq!(db.balance_of(&Account::Miner(a.clone())).await.unwrap().unwrap().total_paid, Amount::ZERO, "sent is not yet paid");

    db.mark_payout_confirmed(id, 3).await.unwrap();
    assert_eq!(db.get_payout(id).await.unwrap().unwrap().status, PayoutStatus::Confirmed);
    let balance = db.balance_of(&Account::Miner(a)).await.unwrap().unwrap();
    assert_eq!((balance.balance, balance.total_paid), (amount(600), amount(400)));
}

#[tokio::test]
async fn a_failed_payout_puts_the_money_back_and_keeps_the_reason() {
    let (db, a) = funded(1_000).await;
    let id = db.create_pending_payouts(&[payout("k", &a, 400)], 1).await.unwrap()[0].id;
    db.fail_payout(id, "the node refused it", 5).await.unwrap();
    let failed = db.get_payout(id).await.unwrap().unwrap();
    assert_eq!((failed.status, failed.note.as_deref()), (PayoutStatus::Failed, Some("the node refused it")));
    let balance = db.balance_of(&Account::Miner(a)).await.unwrap().unwrap();
    assert_eq!((balance.balance, balance.total_paid), (amount(1_000), Amount::ZERO));
}

#[tokio::test]
async fn a_sent_payout_can_still_be_failed_but_a_confirmed_one_cannot() {
    let (db, a) = funded(1_000).await;
    let ids: Vec<i64> =
        db.create_pending_payouts(&[payout("k1", &a, 100), payout("k2", &a, 100)], 1).await.unwrap().iter().map(|p| p.id).collect();
    db.mark_payout_sent(ids[0], "t1", 2).await.unwrap();
    db.fail_payout(ids[0], "never confirmed", 3).await.unwrap();
    db.mark_payout_sent(ids[1], "t2", 2).await.unwrap();
    db.mark_payout_confirmed(ids[1], 3).await.unwrap();
    let error = db.fail_payout(ids[1], "too late", 4).await.unwrap_err();
    assert!(matches!(error, DbError::WrongPayoutState { .. }), "{error}");
}

#[tokio::test]
async fn payout_states_cannot_be_skipped_or_repeated() {
    let (db, a) = funded(1_000).await;
    let id = db.create_pending_payouts(&[payout("k", &a, 100)], 1).await.unwrap()[0].id;

    let error = db.mark_payout_confirmed(id, 2).await.unwrap_err();
    assert!(
        matches!(&error, DbError::WrongPayoutState { found, .. } if found == "pending"),
        "pending cannot jump to confirmed: {error}"
    );
    db.mark_payout_sent(id, "t", 2).await.unwrap();
    let error = db.mark_payout_sent(id, "t", 3).await.unwrap_err();
    assert!(matches!(&error, DbError::WrongPayoutState { found, .. } if found == "sent"), "{error}");
    assert!(matches!(db.mark_payout_sent(999, "t", 3).await.unwrap_err(), DbError::UnknownPayout(999)));
    assert!(matches!(db.fail_payout(999, "x", 3).await.unwrap_err(), DbError::UnknownPayout(999)));
    assert_eq!(db.balance_of(&Account::Miner(a)).await.unwrap().unwrap().total_paid, Amount::ZERO, "the refused confirm paid nothing");
}

#[tokio::test]
async fn payouts_can_be_listed_by_state_oldest_first() {
    let (db, a) = funded(1_000).await;
    let ids: Vec<i64> = db
        .create_pending_payouts(&[payout("k1", &a, 10), payout("k2", &a, 10), payout("k3", &a, 10)], 1)
        .await
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();
    db.mark_payout_sent(ids[1], "t", 2).await.unwrap();
    let pending: Vec<i64> = db.payouts_with_status(PayoutStatus::Pending).await.unwrap().iter().map(|p| p.id).collect();
    assert_eq!(pending, [ids[0], ids[2]]);
    assert_eq!(db.payouts_with_status(PayoutStatus::Sent).await.unwrap().len(), 1);
    assert!(db.payouts_with_status(PayoutStatus::Failed).await.unwrap().is_empty());
}

// ---- meta ---------------------------------------------------------------

#[tokio::test]
async fn meta_values_round_trip_and_can_be_replaced() {
    let db = db().await;
    assert_eq!(db.meta_get("k").await.unwrap(), None);
    db.meta_set("k", "one").await.unwrap();
    db.meta_set("k", "two").await.unwrap();
    assert_eq!(db.meta_get("k").await.unwrap().as_deref(), Some("two"));
}

// ---- locking ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_writers_and_readers_on_a_real_file_never_meet_a_lock_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("pool.db")).await.unwrap();

    let mut tasks = Vec::new();
    for writer in 0..8u8 {
        let db = db.clone();
        tasks.push(tokio::spawn(async move {
            let who = addr(writer);
            for batch in 0..20u64 {
                let shares: Vec<NewShare> = (0..50).map(|i| share(&who, batch * 50 + i, 1)).collect();
                db.insert_shares(&shares).await.expect("a write failed");
                db.upsert_miner(&who, batch).await.expect("a miner write failed");
            }
        }));
    }
    for _ in 0..4 {
        let db = db.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..100 {
                db.weights_in_window(0, 10_000).await.expect("a read failed");
                db.count_shares().await.expect("a count failed");
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(db.count_shares().await.unwrap(), 8 * 20 * 50);
    assert_eq!(db.count_miners().await.unwrap(), 8);
    db.close().await;
}

#[tokio::test]
async fn the_read_pool_cannot_write() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("pool.db")).await.unwrap();
    let error = sqlx::query("INSERT INTO meta (key, value) VALUES ('x', 'y')").execute(db.read_pool()).await.unwrap_err();
    assert!(error.to_string().to_lowercase().contains("readonly"), "{error}");
    db.close().await;
}

// ---- payouts in batches, the watch address, counts and locks ---------------

#[tokio::test]
async fn a_batch_goes_to_sent_together_with_its_transaction_id_and_watch_address() {
    let (db, a) = funded(1_000).await;
    let ids: Vec<i64> =
        db.create_pending_payouts(&[payout("k1", &a, 100), payout("k2", &a, 200)], 1).await.unwrap().iter().map(|p| p.id).collect();
    db.mark_payouts_sent(&ids, "txabc", Some("fake:change7"), 2).await.unwrap();
    for id in &ids {
        let p = db.get_payout(*id).await.unwrap().unwrap();
        assert_eq!(
            (p.status, p.txid.as_deref(), p.watch_address.as_deref(), p.updated_ts_ms),
            (PayoutStatus::Sent, Some("txabc"), Some("fake:change7"), 2)
        );
    }
}

#[tokio::test]
async fn a_batch_that_has_one_wrong_member_changes_none_of_them() {
    let (db, a) = funded(1_000).await;
    let ids: Vec<i64> =
        db.create_pending_payouts(&[payout("k1", &a, 100), payout("k2", &a, 200)], 1).await.unwrap().iter().map(|p| p.id).collect();
    db.mark_payout_sent(ids[1], "t", 2).await.unwrap(); // the second is already sent
    let error = db.mark_payouts_sent(&ids, "txabc", None, 3).await.unwrap_err();
    assert!(matches!(error, DbError::WrongPayoutState { .. }), "{error}");
    assert_eq!(db.get_payout(ids[0]).await.unwrap().unwrap().status, PayoutStatus::Pending, "the first was rolled back");
}

#[tokio::test]
async fn a_batch_is_confirmed_together_and_counts_as_paid() {
    let (db, a) = funded(1_000).await;
    let ids: Vec<i64> =
        db.create_pending_payouts(&[payout("k1", &a, 100), payout("k2", &a, 200)], 1).await.unwrap().iter().map(|p| p.id).collect();
    db.mark_payouts_sent(&ids, "tx", None, 2).await.unwrap();
    db.confirm_payouts(&ids, 3).await.unwrap();
    let balance = db.balance_of(&Account::Miner(a)).await.unwrap().unwrap();
    assert_eq!((balance.balance, balance.total_paid), (amount(700), amount(300)));
}

#[tokio::test]
async fn a_batch_is_failed_together_and_every_amount_comes_back() {
    let (db, a) = funded(1_000).await;
    let ids: Vec<i64> =
        db.create_pending_payouts(&[payout("k1", &a, 100), payout("k2", &a, 200)], 1).await.unwrap().iter().map(|p| p.id).collect();
    db.fail_payouts(&ids, "cancelled", 2).await.unwrap();
    let balance = db.balance_of(&Account::Miner(a)).await.unwrap().unwrap();
    assert_eq!((balance.balance, balance.total_paid), (amount(1_000), Amount::ZERO));
    assert_eq!(db.payouts_with_status(PayoutStatus::Failed).await.unwrap().len(), 2);
}

#[tokio::test]
async fn payouts_are_counted_per_miner_in_every_state() {
    let (db, a) = funded(1_000).await;
    assert_eq!(db.payout_count(&a).await.unwrap(), 0);
    let ids: Vec<i64> = db.create_pending_payouts(&[payout("k1", &a, 100)], 1).await.unwrap().iter().map(|p| p.id).collect();
    db.fail_payouts(&ids, "x", 2).await.unwrap();
    db.create_pending_payouts(&[payout("k2", &a, 100)], 3).await.unwrap();
    assert_eq!(db.payout_count(&a).await.unwrap(), 2, "a failed payout still counts");
    assert_eq!(db.payout_count(&addr(9)).await.unwrap(), 0);
}

#[tokio::test]
async fn a_lock_keeps_a_second_holder_out_until_it_is_given_back_or_runs_out() {
    let db = db().await;
    assert!(db.try_lock("payout", "alice", 1_000, 500).await.unwrap());
    assert!(!db.try_lock("payout", "bob", 1_100, 500).await.unwrap(), "held by alice");
    assert!(db.try_lock("payout", "alice", 1_100, 500).await.unwrap(), "the holder may extend it");
    assert!(!db.try_lock("payout", "bob", 1_599, 500).await.unwrap(), "extended to 1600");
    assert!(db.try_lock("payout", "bob", 1_600, 500).await.unwrap(), "a lock that ran out is free");
    db.unlock("payout", "alice").await.unwrap(); // not alice's any more: nothing happens
    assert!(!db.try_lock("payout", "alice", 1_700, 500).await.unwrap(), "bob still holds it");
    db.unlock("payout", "bob").await.unwrap();
    assert!(db.try_lock("payout", "alice", 1_700, 500).await.unwrap(), "given back");
    assert!(db.try_lock("other", "bob", 1_700, 500).await.unwrap(), "locks are separate by name");
}

#[tokio::test]
async fn the_second_migration_is_applied_and_a_reopen_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pool.db");
    let first = Db::open(&path).await.unwrap();
    assert_eq!(migrations_applied(&first).await, 2);
    first.close().await;
    let second = Db::open(&path).await.unwrap();
    assert_eq!(migrations_applied(&second).await, 2);
    second.close().await;
}
