//! What every command that touches the pool's data needs to know.

use clap::Args;
use pool_chain::{Chain, WalletSecret, WalletSettings};
use std::path::PathBuf;

/// The pool's database file, inside its data directory.
pub const DB_FILENAME: &str = "pool.db";

/// The name of the environment variable that holds the wallet's password. The
/// password is never taken from the command line, where it would show up in
/// the list of running programs.
pub const SECRET_VARIABLE: &str = "MARIGOLD_POOL_WALLET_SECRET";

#[derive(Args, Clone)]
pub struct PoolArgs {
    /// The pool's data directory: its database, and the wallet in a folder inside it.
    #[arg(long)]
    pub data_dir: PathBuf,
    /// The network the pool runs on.
    #[arg(long, default_value = "testnet-10")]
    pub network: String,
    /// The node's wRPC address. By default the node on this machine, on the network's usual port.
    #[arg(long)]
    pub wrpc: Option<String>,
}

impl PoolArgs {
    pub fn wallet_settings<C: Chain>(&self, secret: Option<String>) -> Result<WalletSettings, String> {
        let secret = secret
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("The wallet password is missing. Put it in the environment variable {SECRET_VARIABLE} (a service reads it from its environment file)."))?;
        let port = C::default_wrpc_port(&self.network).ok_or_else(|| unknown_network(&self.network))?;
        Ok(WalletSettings {
            network: self.network.clone(),
            wrpc_url: self.wrpc.clone().unwrap_or_else(|| format!("ws://127.0.0.1:{port}")),
            data_dir: self.data_dir.clone(),
            secret: WalletSecret::new(secret),
        })
    }
}

pub fn unknown_network(network: &str) -> String {
    format!("There is no network called \"{network}\". Use mainnet or testnet-10.")
}

/// Wait until the program is asked to stop: Ctrl-C, or SIGTERM from systemd.
#[cfg(unix)]
pub async fn wait_for_stop() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        },
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(not(unix))]
pub async fn wait_for_stop() {
    let _ = tokio::signal::ctrl_c().await;
}
