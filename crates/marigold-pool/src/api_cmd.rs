//! `marigold-pool api`: serve the pool's API and dashboard from its database.
//! It only reads. Run on its own it shows what the database holds; the full
//! pool serves the same pages while it runs.

use crate::common::{DB_FILENAME, wait_for_stop};
use clap::Args;
use pool_accountant::{AccountantMetrics, DEFAULT_FEE_BP, DEFAULT_WINDOW_DAA};
use pool_api::{ApiState, system_clock};
use pool_chain::Chain;
use pool_db::Db;
use pool_domain::Amount;
use pool_payout::DEFAULT_MIN_PAYOUT_COINS;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Args)]
pub struct ApiArgs {
    /// The pool's data directory.
    #[arg(long)]
    pub data_dir: PathBuf,
    /// The network the pool runs on.
    #[arg(long, default_value = "testnet-10")]
    pub network: String,
    /// Where to listen. Keep it on this machine and put a web server in front for the public.
    #[arg(long, default_value = "127.0.0.1:8788")]
    pub bind: SocketAddr,
    /// The address block rewards are paid to, shown on the page.
    #[arg(long, default_value = "")]
    pub pool_address: String,
    /// The pool's fee in basis points (100 is one percent), shown on the page.
    #[arg(long, default_value_t = DEFAULT_FEE_BP)]
    pub fee_bp: u16,
    /// The least a miner must be owed, in whole coins, shown on the page.
    #[arg(long, default_value_t = DEFAULT_MIN_PAYOUT_COINS)]
    pub min_payout_coins: u64,
    /// The share window in DAA steps, shown on the page.
    #[arg(long, default_value_t = DEFAULT_WINDOW_DAA)]
    pub window_daa: u64,
    /// A node to check for the health page, such as grpc://127.0.0.1:26210. Without one the node is not checked.
    #[arg(long)]
    pub node: Option<String>,
}

pub async fn run<C: Chain + 'static>(args: ApiArgs) -> Result<String, String> {
    C::address_prefix(&args.network).ok_or_else(|| crate::common::unknown_network(&args.network))?;
    let min_payout = Amount::from_coins(args.min_payout_coins).ok_or("That minimum payout is too large.")?;
    let db = Db::open(&args.data_dir.join(DB_FILENAME))
        .await
        .map_err(|e| format!("Could not open the pool's database in {}: {e}", args.data_dir.display()))?;
    let node = match &args.node {
        Some(url) => Some(C::connect_node(url).await.map_err(|e| e.to_string())?),
        None => None,
    };
    let listener = tokio::net::TcpListener::bind(args.bind).await.map_err(|e| format!("Could not listen on {}: {e}", args.bind))?;
    let state = Arc::new(ApiState {
        db: db.clone(),
        network: args.network.clone(),
        pool_address: args.pool_address.clone(),
        fee_bp: args.fee_bp,
        min_payout,
        window_daa: args.window_daa,
        metrics: Arc::new(AccountantMetrics::default()),
        node,
        clock: system_clock(),
    });

    let (stop, shutdown) = watch::channel(false);
    tokio::spawn(async move {
        wait_for_stop().await;
        let _ = stop.send(true);
    });
    eprintln!("Serving the pool's pages on http://{}/ (read-only). Press Ctrl-C to stop.", args.bind);
    let served = pool_api::serve::<C>(state, listener, shutdown).await;
    db.close().await;
    served.map(|_| "Stopped.".to_owned()).map_err(|e| format!("The server stopped with a problem: {e}"))
}
