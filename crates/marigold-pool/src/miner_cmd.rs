//! `marigold-pool miner reset`: the operator's way out when a miner has lost
//! their password, or when someone else chose a password for their address first.

use crate::common::DB_FILENAME;
use crate::config::PoolConfig;
use clap::{Args, Subcommand};
use pool_chain::Chain;
use pool_db::Db;
use pool_domain::MinerAddress;
use std::path::PathBuf;

#[derive(Args)]
pub struct MinerArgs {
    #[command(subcommand)]
    pub command: MinerCommand,
}

#[derive(Subcommand)]
pub enum MinerCommand {
    /// Remove a miner's password and settings. They go back to the pool's defaults and can choose a new password on the pool's page. Their money and their payments are not touched.
    Reset(ResetArgs),
}

#[derive(Args)]
pub struct ResetArgs {
    /// The miner's address.
    #[arg(long)]
    pub address: String,
    /// The pool's settings file: its data directory and network are used.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// The pool's data directory (not needed with --config).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    /// The network (default: the settings file's, or testnet-10).
    #[arg(long)]
    pub network: Option<String>,
}

pub async fn run<C: Chain>(args: MinerArgs) -> Result<String, String> {
    match args.command {
        MinerCommand::Reset(reset) => reset_account::<C>(reset).await,
    }
}

async fn reset_account<C: Chain>(args: ResetArgs) -> Result<String, String> {
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
    let address = MinerAddress::parse::<C>(&network, &args.address).map_err(|e| e.to_string())?;

    let database = data_dir.join(DB_FILENAME);
    if !database.exists() {
        return Err(format!("There is no pool database in {}. Check where the pool keeps its data.", data_dir.display()));
    }
    let db = Db::open(&database).await.map_err(|e| format!("Could not open the pool's database: {e}"))?;
    let result = db.reset_miner_account(&address).await.map_err(|e| e.to_string());
    db.close().await;
    Ok(if result? {
        format!(
            "The password and settings for {address} were removed. They are back at the pool's defaults and can choose a new password on the pool's page."
        )
    } else {
        format!("{address} had no password or settings, so nothing was changed.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pool_chain::testing::FakeChain;

    async fn pool_with_account(dir: &std::path::Path) {
        let db = Db::open(&dir.join(DB_FILENAME)).await.unwrap();
        db.create_miner_account(&MinerAddress::from_trusted("fake:m1".into()), "hash", None, 1).await.unwrap();
        db.close().await;
    }

    fn args(dir: &std::path::Path, address: &str) -> ResetArgs {
        ResetArgs { address: address.into(), config: None, data_dir: Some(dir.to_path_buf()), network: Some("fakenet".into()) }
    }

    #[tokio::test]
    async fn a_reset_removes_the_account_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        pool_with_account(dir.path()).await;
        let text = reset_account::<FakeChain>(args(dir.path(), "fake:m1")).await.unwrap();
        assert!(text.starts_with("The password and settings for fake:m1 were removed."), "{text}");
        let db = Db::open(&dir.path().join(DB_FILENAME)).await.unwrap();
        assert!(db.miner_account(&MinerAddress::from_trusted("fake:m1".into())).await.unwrap().is_none());
        db.close().await;
    }

    #[tokio::test]
    async fn resetting_an_address_with_nothing_set_changes_nothing_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        pool_with_account(dir.path()).await;
        let text = reset_account::<FakeChain>(args(dir.path(), "fake:other")).await.unwrap();
        assert_eq!(text, "fake:other had no password or settings, so nothing was changed.");
    }

    #[tokio::test]
    async fn a_bad_address_or_a_missing_database_is_refused_in_words() {
        let dir = tempfile::tempdir().unwrap();
        let error = reset_account::<FakeChain>(args(dir.path(), "nonsense")).await.unwrap_err();
        assert_eq!(error, "That does not look like an address. It starts with fake:.");
        let error = reset_account::<FakeChain>(args(dir.path(), "fake:m1")).await.unwrap_err();
        assert!(error.starts_with("There is no pool database in"), "{error}");
        assert!(!dir.path().join(DB_FILENAME).exists(), "and it did not make one");
    }
}
