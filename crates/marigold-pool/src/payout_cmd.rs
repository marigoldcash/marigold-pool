//! `marigold-pool payout --now`: pay what miners are owed, straight away.

use crate::common::{DB_FILENAME, PoolArgs};
use clap::Args;
use pool_chain::Chain;
use pool_db::{Db, PayoutStatus};
use pool_domain::Amount;
use pool_payout::{CycleSummary, DEFAULT_MIN_PAYOUT_COINS, PayoutError, PayoutSettings, describe, run_cycle};

#[derive(Args)]
pub struct PayoutArgs {
    #[command(flatten)]
    pub pool: PoolArgs,
    /// Run a payout now. The pool also pays by itself on its schedule.
    #[arg(long)]
    pub now: bool,
    /// The least a miner must be owed, in whole coins, before they are paid.
    #[arg(long, default_value_t = DEFAULT_MIN_PAYOUT_COINS)]
    pub min_payout_coins: u64,
}

pub async fn run<C: Chain>(args: PayoutArgs, secret: Option<String>, now_ms: u64) -> Result<String, String> {
    if !args.now {
        return Ok("This pays miners straight away. Add --now to do it. The pool also pays by itself on its schedule.".to_owned());
    }
    let min = Amount::from_coins(args.min_payout_coins).ok_or("That minimum payout is too large.")?;

    std::fs::create_dir_all(&args.pool.data_dir)
        .map_err(|e| format!("Could not make the data directory {}: {e}", args.pool.data_dir.display()))?;
    let db = Db::open(&args.pool.data_dir.join(DB_FILENAME)).await.map_err(|e| format!("Could not open the pool's database: {e}"))?;

    let result = payout::<C>(&db, &args, min, secret, now_ms).await;
    db.close().await;
    result
}

async fn payout<C: Chain>(db: &Db, args: &PayoutArgs, min: Amount, secret: Option<String>, now_ms: u64) -> Result<String, String> {
    // Nobody to pay and nothing half-done: say so without needing the wallet,
    // its password or the node.
    let unfinished = db.payouts_with_status(PayoutStatus::Pending).await.map_err(|e| e.to_string())?.len()
        + db.payouts_with_status(PayoutStatus::Sent).await.map_err(|e| e.to_string())?.len();
    let owed = db.balances_eligible(min).await.map_err(|e| e.to_string())?;
    if unfinished == 0 && owed.is_empty() {
        let someone_owed_less = !db.balances_eligible(Amount::ZERO).await.map_err(|e| e.to_string())?.is_empty();
        let summary = CycleSummary { nobody_owed: true, someone_owed_less, ..CycleSummary::default() };
        return Ok(describe::<C>(&summary, min));
    }

    let settings = args.pool.wallet_settings::<C>(secret)?;
    let payer = C::open_payer(&settings).await.map_err(|e| e.to_string())?;
    match run_cycle(db, payer.as_ref(), &PayoutSettings::new(min), now_ms).await {
        Ok(summary) => Ok(describe::<C>(&summary, min)),
        Err(PayoutError::Busy) => Err(PayoutError::Busy.to_string()),
        Err(other) => Err(format!("The payout run did not finish: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pool_chain::testing::FakeChain;
    use pool_domain::{Account, MinerAddress};

    fn args(dir: &std::path::Path, now: bool) -> PayoutArgs {
        PayoutArgs { pool: PoolArgs { data_dir: dir.to_path_buf(), network: "fakenet".into(), wrpc: None }, now, min_payout_coins: 10 }
    }

    #[tokio::test]
    async fn without_now_it_explains_instead_of_paying() {
        let dir = tempfile::tempdir().unwrap();
        let text = run::<FakeChain>(args(dir.path(), false), None, 1).await.unwrap();
        assert!(text.contains("Add --now"), "{text}");
        assert!(!dir.path().join(DB_FILENAME).exists(), "and it touches nothing");
    }

    #[tokio::test]
    async fn against_an_empty_database_it_says_in_plain_words_that_nobody_is_owed_anything() {
        let dir = tempfile::tempdir().unwrap();
        // No password, no wallet, no node: none of them are needed to know there is nobody to pay.
        let text = run::<FakeChain>(args(dir.path(), true), None, 1).await.unwrap();
        assert_eq!(text, "Nobody is owed anything yet, so there is nothing to pay.");
        assert!(dir.path().join(DB_FILENAME).exists(), "the database was made");
    }

    #[tokio::test]
    async fn small_balances_get_their_own_explanation() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(DB_FILENAME)).await.unwrap();
        db.record_reward_if_new("r:0", Amount::new(100), 1, 0).await.unwrap();
        db.apply_allocation("r:0", &[(Account::Miner(MinerAddress::from_trusted("fake:m1".into())), Amount::new(100))], 1)
            .await
            .unwrap();
        db.close().await;
        let text = run::<FakeChain>(args(dir.path(), true), None, 1).await.unwrap();
        assert!(text.starts_with("Nobody is owed 10.00 TST or more yet"), "{text}");
    }

    #[tokio::test]
    async fn when_someone_is_owed_the_wallet_password_is_asked_for_in_words() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(DB_FILENAME)).await.unwrap();
        db.record_reward_if_new("r:0", Amount::new(20 * 100_000_000), 1, 0).await.unwrap();
        db.apply_allocation(
            "r:0",
            &[(Account::Miner(MinerAddress::from_trusted("fake:m1".into())), Amount::new(20 * 100_000_000))],
            1,
        )
        .await
        .unwrap();
        db.close().await;
        let error = run::<FakeChain>(args(dir.path(), true), None, 1).await.unwrap_err();
        assert!(error.contains("MARIGOLD_POOL_WALLET_SECRET"), "{error}");
    }
}
