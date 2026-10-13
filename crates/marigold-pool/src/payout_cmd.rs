//! `marigold-pool payout --now`: pay what miners are owed, straight away.

use crate::common::{DB_FILENAME, PoolArgs};
use crate::config::PoolConfig;
use clap::Args;
use pool_chain::Chain;
use pool_db::{Db, PayoutStatus};
use pool_domain::Amount;
use pool_payout::{CycleSummary, DEFAULT_MIN_PAYOUT_COINS, MIN_PAYOUT_FLOOR_COINS, PayoutError, PayoutSettings, describe, run_cycle};
use std::path::PathBuf;

#[derive(Args)]
pub struct PayoutArgs {
    /// The pool's settings file: its data directory, network, node and minimum payout are used.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// The pool's data directory (not needed with --config).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    /// The network (default: the settings file's, or testnet-10).
    #[arg(long)]
    pub network: Option<String>,
    /// The node's wRPC address (default: the settings file's, or the usual port on this machine).
    #[arg(long)]
    pub wrpc: Option<String>,
    /// Run a payout now. The pool also pays by itself on its schedule.
    #[arg(long)]
    pub now: bool,
    /// The least a miner must be owed, in coins, decimals allowed, and not below 10 (default: the settings file's, or 10).
    #[arg(long)]
    pub min_payout_coins: Option<String>,
}

/// Where the data is and how much counts as owed enough, from the flags and the settings file.
fn resolve<C: Chain>(args: &PayoutArgs) -> Result<(PoolArgs, Amount), String> {
    let config = match &args.config {
        Some(path) => Some(PoolConfig::load::<C>(path).map_err(|e| e.to_string())?),
        None => None,
    };
    let data_dir = args
        .data_dir
        .clone()
        .or_else(|| config.as_ref().map(|c| c.data_dir.clone()))
        .ok_or("Say where the pool keeps its data: --config with the settings file, or --data-dir.")?;
    let network =
        args.network.clone().or_else(|| config.as_ref().map(|c| c.network.clone())).unwrap_or_else(|| "testnet-10".to_owned());
    let wrpc = args.wrpc.clone().or_else(|| config.as_ref().map(|c| c.node_wrpc.clone()));

    let floor = Amount::from_coins(MIN_PAYOUT_FLOOR_COINS).unwrap_or(Amount::ZERO);
    let min = match &args.min_payout_coins {
        Some(text) => {
            let asked = Amount::parse_coins(text)
                .ok_or_else(|| format!("{text:?} is not a minimum payout. Write a number of coins, like 10 or 25.5."))?;
            if asked < floor {
                return Err(format!(
                    "{text} is below the lowest minimum payout the pool allows, {}. Smaller payments do not work.",
                    floor.display::<C>()
                ));
            }
            asked
        }
        None => match &config {
            Some(c) => c.min_payout,
            None => Amount::from_coins(DEFAULT_MIN_PAYOUT_COINS).ok_or("That minimum payout is too large.")?,
        },
    };
    Ok((PoolArgs { data_dir, network, wrpc }, min))
}

pub async fn run<C: Chain>(args: PayoutArgs, secret: Option<String>, now_ms: u64) -> Result<String, String> {
    if !args.now {
        return Ok("This pays miners straight away. Add --now to do it. The pool also pays by itself on its schedule.".to_owned());
    }
    let (pool, min) = resolve::<C>(&args)?;

    std::fs::create_dir_all(&pool.data_dir)
        .map_err(|e| format!("Could not make the data directory {}: {e}", pool.data_dir.display()))?;
    let db = Db::open(&pool.data_dir.join(DB_FILENAME)).await.map_err(|e| format!("Could not open the pool's database: {e}"))?;

    let result = payout::<C>(&db, &pool, min, secret, now_ms).await;
    db.close().await;
    result
}

async fn payout<C: Chain>(db: &Db, pool: &PoolArgs, min: Amount, secret: Option<String>, now_ms: u64) -> Result<String, String> {
    // Nobody to pay and nothing half-done: say so without needing the wallet,
    // its password or the node.
    let unfinished = db.payouts_with_status(PayoutStatus::Pending).await.map_err(|e| e.to_string())?.len()
        + db.payouts_with_status(PayoutStatus::Sent).await.map_err(|e| e.to_string())?.len();
    let owed = db.balances_eligible(min).await.map_err(|e| e.to_string())?;
    if unfinished == 0 && owed.is_empty() {
        let someone_owed_less = !db.balances_owed().await.map_err(|e| e.to_string())?.is_empty();
        let summary = CycleSummary { nobody_owed: true, someone_owed_less, ..CycleSummary::default() };
        return Ok(describe::<C>(&summary, min));
    }

    let settings = pool.wallet_settings::<C>(secret)?;
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

    const COIN: u64 = 100_000_000;

    fn args(dir: &std::path::Path, now: bool) -> PayoutArgs {
        PayoutArgs {
            config: None,
            data_dir: Some(dir.to_path_buf()),
            network: Some("fakenet".into()),
            wrpc: None,
            now,
            min_payout_coins: None,
        }
    }

    async fn owe(dir: &std::path::Path, units: u64) {
        let db = Db::open(&dir.join(DB_FILENAME)).await.unwrap();
        db.record_reward_if_new("r:0", Amount::new(units), 1, 0).await.unwrap();
        db.apply_allocation("r:0", &[(Account::Miner(MinerAddress::from_trusted("fake:m1".into())), Amount::new(units))], 1)
            .await
            .unwrap();
        db.close().await;
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
        owe(dir.path(), 100).await;
        let text = run::<FakeChain>(args(dir.path(), true), None, 1).await.unwrap();
        assert!(text.starts_with("Nobody has reached their payout level yet") && text.contains("starts at 10.00 TST"), "{text}");
    }

    #[tokio::test]
    async fn when_someone_is_owed_the_wallet_password_is_asked_for_in_words() {
        let dir = tempfile::tempdir().unwrap();
        owe(dir.path(), 20 * COIN).await;
        let error = run::<FakeChain>(args(dir.path(), true), None, 1).await.unwrap_err();
        assert!(error.contains("MARIGOLD_POOL_WALLET_SECRET"), "{error}");
    }

    #[tokio::test]
    async fn the_minimum_can_be_raised_on_the_command_line_with_decimals() {
        let dir = tempfile::tempdir().unwrap();
        owe(dir.path(), 12 * COIN).await; // owed twelve: over the default ten
        let mut raised = args(dir.path(), true);
        raised.min_payout_coins = Some("12.5".into());
        let text = run::<FakeChain>(raised, None, 1).await.unwrap();
        assert!(
            text.starts_with("Nobody has reached their payout level yet") && text.contains("starts at 12.50 TST"),
            "not enough at the higher minimum: {text}"
        );
    }

    #[tokio::test]
    async fn a_minimum_below_ten_or_nonsense_is_refused_in_words() {
        let dir = tempfile::tempdir().unwrap();
        for low in ["9.99999999", "1", "0.5", "0"] {
            let mut a = args(dir.path(), true);
            a.min_payout_coins = Some(low.into());
            let error = run::<FakeChain>(a, None, 1).await.unwrap_err();
            assert!(error.contains("below the lowest minimum payout the pool allows, 10.00 TST"), "{low:?}: {error}");
        }
        for bad in ["ten", "-1", "", "1e1"] {
            let mut a = args(dir.path(), true);
            a.min_payout_coins = Some(bad.into());
            let error = run::<FakeChain>(a, None, 1).await.unwrap_err();
            assert!(error.contains("is not a minimum payout"), "{bad:?}: {error}");
        }
    }

    #[tokio::test]
    async fn it_needs_to_be_told_where_the_data_is() {
        let mut a = args(std::path::Path::new("/unused"), true);
        a.data_dir = None;
        assert!(run::<FakeChain>(a, None, 1).await.unwrap_err().starts_with("Say where the pool keeps its data"));
    }

    #[tokio::test]
    async fn the_settings_file_supplies_the_data_directory_and_the_minimum() {
        let dir = tempfile::tempdir().unwrap();
        owe(dir.path(), 12 * COIN).await;
        let config_path = dir.path().join("pool.toml");
        std::fs::write(
            &config_path,
            format!(
                "[chain]\nname = \"marigold\"\n[network]\nid = \"fakenet\"\n[pool]\naddress = \"fake:pool\"\ndata_dir = \"{}\"\nmin_payout_tst = 11\n[stratum]\n[[stratum.instances]]\nport = 5570\nmin_share_diff = 1\n",
                dir.path().display()
            ),
        )
        .unwrap();
        let a = PayoutArgs { config: Some(config_path), data_dir: None, network: None, wrpc: None, now: true, min_payout_coins: None };
        let error = run::<FakeChain>(a, None, 1).await.unwrap_err();
        assert!(
            error.contains("MARIGOLD_POOL_WALLET_SECRET"),
            "the file's lower minimum (0.5) means the one-coin balance is payable: {error}"
        );
    }
}
