//! Fill a database file with a small, known pool, for trying the API by hand:
//!
//!     cargo run -p pool-db --example seed_demo -- /path/to/pool.db
//!
//! Never point it at a real pool's database. It is an example, so it is not part
//! of the pool program.
//!
//! The two addresses are testnet addresses that belong to nobody's pool.

use pool_db::{Db, NewBlock, NewPayout, NewShare};
use pool_domain::{Account, Amount, MinerAddress, ShareWeight, WorkerName};
use std::path::PathBuf;

const ALICE: &str = "marigoldtest:qqfc69eulu3v8qamcasxqcx3wxvsfzjkv73fau4exk5kem70nqjsqgm3wm9g8";
const BOB: &str = "marigoldtest:qq5cen0cx8y7qmzqljmyywlnchu7fv4jtad9rdfq0x5d234dq8gzgx4vcrnyr";
const COIN: u64 = 100_000_000;

#[tokio::main]
async fn main() {
    let path: PathBuf = std::env::args().nth(1).expect("usage: seed_demo <path to a new pool.db>").into();
    if path.exists() {
        eprintln!("{} already exists; this only fills a new database.", path.display());
        std::process::exit(2);
    }
    let db = Db::open(&path).await.expect("open the database");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let alice = MinerAddress::from_trusted(ALICE.into());
    let bob = MinerAddress::from_trusted(BOB.into());

    let share = |who: &MinerAddress, worker: &str, age_ms: u64, hash_value: f64| NewShare {
        address: who.clone(),
        worker: WorkerName::new(worker),
        weight: ShareWeight::new(1_000),
        hash_value,
        daa_score: 1,
        ts_ms: now - age_ms,
    };
    db.insert_shares(&[
        share(&alice, "rig1", 30_000, 6_000.0),
        share(&alice, "rig1", 20_000, 4_000.0),
        share(&alice, "rig2", 1_000, 5_000.0),
        share(&bob, "x", 3_600_000, 2_000.0),
    ])
    .await
    .unwrap();
    for (hash, who, worker, age_ms, blue) in
        [("b1", &alice, "rig1", 60_000, Some(true)), ("b2", &bob, "x", 3_600_000, Some(false)), ("b3", &alice, "rig2", 5_000, None)]
    {
        db.insert_block(&NewBlock {
            hash: hash.into(),
            address: who.clone(),
            worker: WorkerName::new(worker),
            blue_score: 7,
            daa_score: 8,
            nonce: 9,
            ts_ms: now - age_ms,
        })
        .await
        .unwrap();
        if let Some(blue) = blue {
            db.set_block_blue(hash, blue).await.unwrap();
        }
    }
    db.record_reward_if_new("r1:0", Amount::new(10 * COIN), 1, 0).await.unwrap();
    db.apply_allocation(
        "r1:0",
        &[
            (Account::Pool, Amount::new(COIN / 10)),
            (Account::Miner(alice.clone()), Amount::new(6 * COIN)),
            (Account::Miner(bob.clone()), Amount::new(390_000_000)),
        ],
        1,
    )
    .await
    .unwrap();
    let rows = db
        .create_pending_payouts(
            &[NewPayout { idempotency_key: "payout:bob:1".into(), address: bob.clone(), amount: Amount::new(390_000_000) }],
            now - 1_000,
        )
        .await
        .unwrap();
    db.mark_payouts_sent(&[rows[0].id], "txbob", Some("marigoldtest:change"), now - 900).await.unwrap();
    db.confirm_payouts(&[rows[0].id], now - 800).await.unwrap();
    db.close().await;

    println!("seeded {}", path.display());
    println!("expected on /api/pool: miners_online 1, workers_online 2, shares_10m 3, hashrate_hps 25, blocks_24h 3,");
    println!("  owed_to_miners 6.00 MAGLD, min payout 10.00 MAGLD (when started with the defaults)");
    println!("alice (online): {ALICE}");
    println!("bob (paid, offline): {BOB}");
}
