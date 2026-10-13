//! `marigold-pool serve`: the whole pool in one process.
//!
//! `run_pool` takes its node, wallet and bridge as ready-made parts, so tests
//! can run the entire pipeline with fakes. `serve_command` builds the real parts
//! for the chain this binary was made for.

use crate::common::{DB_FILENAME, SECRET_VARIABLE, unknown_network, wait_for_stop};
use crate::config::PoolConfig;
use clap::Args;
use pool_accountant::{
    AccountantMetrics, AllocatorSettings, BroadcastSink, ConsumerSettings, WatcherSettings, run_allocator, run_consumer,
    run_reward_watcher,
};
use pool_api::{ApiState, system_clock};
use pool_chain::{Chain, PoolEvents, SharedLauncher, SharedNode, SharedPayer, WalletSecret, WalletSettings};
use pool_db::Db;
use pool_payout::{PayoutSettings, recover, run_scheduler};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

#[derive(Args)]
pub struct ServeArgs {
    /// The pool's settings file.
    #[arg(long, default_value = "/etc/marigold/pool.toml")]
    pub config: PathBuf,
}

/// The pool's connections to the outside, ready to use.
pub struct Parts {
    pub node: SharedNode,
    pub payer: SharedPayer,
    pub launcher: SharedLauncher,
}

/// How often each part of the pool does its periodic work.
#[derive(Debug, Clone)]
pub struct Timing {
    pub flush_interval: Duration,
    pub prune_interval: Duration,
    pub reward_poll: Duration,
    pub allocate_interval: Duration,
    /// How long to wait for the bridge to close its ports on the way out.
    pub bridge_stop_wait: Duration,
}

impl Timing {
    pub fn production() -> Self {
        Timing {
            flush_interval: Duration::from_millis(250),
            prune_interval: Duration::from_secs(60),
            reward_poll: Duration::from_secs(10),
            allocate_interval: Duration::from_secs(30),
            bridge_stop_wait: Duration::from_secs(20),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Why a task that should run until told to stop ended on its own.
fn ended_by_itself<T, E: std::fmt::Display>(what: &str, result: Result<Result<T, E>, tokio::task::JoinError>) -> String {
    match result {
        Ok(Ok(_)) => format!("The {what} stopped by itself."),
        Ok(Err(why)) => why.to_string(),
        Err(why) => format!("The {what} stopped unexpectedly: {why}"),
    }
}

/// Run the pool until `stop` is ready, or until something that must keep running
/// does not. The database is left open for the caller to close.
pub async fn run_pool<C: Chain + 'static>(
    config: &PoolConfig,
    db: Db,
    parts: Parts,
    timing: Timing,
    stop: impl Future<Output = ()>,
) -> Result<(), String> {
    let maturity = C::coinbase_maturity_daa(&config.network).ok_or_else(|| unknown_network(&config.network))?;

    // Rewards paid to an address the wallet does not control can never be paid out.
    match parts.payer.owns_address(config.pool_address.as_str()).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(format!(
                "The pool address in the settings ({}) does not belong to the pool wallet, so rewards paid to it could never be paid out. Use the address `marigold-pool init` printed, and check data_dir points at that wallet.",
                config.pool_address
            ));
        }
        Err(why) => return Err(why.to_string()),
    }

    // Find a port clash before anything starts.
    let listener = tokio::net::TcpListener::bind(config.api_bind)
        .await
        .map_err(|e| format!("Could not listen for the pool's pages on {}: {e}", config.api_bind))?;

    let metrics = Arc::new(AccountantMetrics::default());
    let sink = Arc::new(BroadcastSink::new());
    let events = sink.subscribe();

    // Payments left half-done by a stop or a crash are sorted out before anything
    // new is sent. If the node is not ready yet this waits for the first payout run.
    let payout_settings = PayoutSettings::new(config.min_payout);
    match recover(&db, parts.payer.as_ref(), &payout_settings, now_ms()).await {
        Ok((0, 0, 0)) => {}
        Ok((cancelled, confirmed, waiting)) => {
            info!("earlier payments: {confirmed} confirmed, {cancelled} cancelled and returned to balances, {waiting} still waiting")
        }
        Err(why) => warn!("could not check earlier payments yet; this will be done before the next payout run: {why}"),
    }

    // Three groups, stopped in this order: what takes work in, what stores it,
    // and what watches and pays.
    let (front_stop, front) = watch::channel(false);
    let (store_stop, store) = watch::channel(false);
    let (back_stop, back) = watch::channel(false);

    let consumer: JoinHandle<()> = tokio::spawn(run_consumer::<C>(
        db.clone(),
        events,
        ConsumerSettings {
            network: config.network.clone(),
            window_daa: config.window_daa,
            flush_interval: timing.flush_interval,
            flush_rows: 500,
            prune_interval: timing.prune_interval,
        },
        Arc::clone(&metrics),
        store,
    ));
    let watcher = tokio::spawn(run_reward_watcher(
        db.clone(),
        parts.node.clone(),
        WatcherSettings {
            pool_address: config.pool_address.to_string(),
            poll_interval: timing.reward_poll,
            colour_window: Duration::from_secs(60),
        },
        Arc::clone(&metrics),
        back.clone(),
    ));
    let allocator = tokio::spawn(run_allocator(
        db.clone(),
        parts.node.clone(),
        AllocatorSettings {
            window_daa: config.window_daa,
            fee_bp: config.fee_bp,
            maturity_daa: maturity,
            interval: timing.allocate_interval,
        },
        Arc::clone(&metrics),
        back.clone(),
    ));
    let scheduler =
        tokio::spawn(run_scheduler(db.clone(), parts.payer.clone(), payout_settings, config.payout_schedule, back.clone()));

    let api_state = Arc::new(ApiState {
        db: db.clone(),
        network: config.network.clone(),
        pool_address: config.pool_address.to_string(),
        fee_bp: config.fee_bp,
        min_payout: config.min_payout,
        window_daa: config.window_daa,
        metrics: Arc::clone(&metrics),
        node: Some(parts.node.clone()),
        clock: system_clock(),
    });
    let mut api: Option<JoinHandle<std::io::Result<()>>> =
        Some(tokio::spawn(pool_api::serve::<C>(api_state, listener, front.clone())));
    let launcher = parts.launcher.clone();
    let bridge_events: Arc<dyn PoolEvents> = sink.clone();
    let mut bridge: Option<JoinHandle<Result<(), pool_chain::BridgeError>>> =
        Some(tokio::spawn(async move { launcher.run(bridge_events, front).await }));

    info!(
        "the pool is running: stratum on {}, pages on http://{}/, paying out on \"{}\"",
        config.stratum.ports.iter().map(|p| p.listen.as_str()).collect::<Vec<_>>().join(", "),
        config.api_bind,
        config.payout_cron
    );

    // Run until told to stop, or until the bridge or the pages stop on their own.
    let outcome: Result<(), String> = tokio::select! {
        _ = stop => Ok(()),
        finished = bridge.as_mut().expect("just set") => {
            bridge = None;
            Err(ended_by_itself("stratum bridge", finished))
        }
        finished = api.as_mut().expect("just set") => {
            api = None;
            Err(ended_by_itself("pool pages", finished))
        }
    };
    match &outcome {
        Ok(()) => info!("stopping"),
        Err(why) => error!("stopping because: {why}"),
    }

    // 1. Stop taking work in, and wait for the ports to close.
    let _ = front_stop.send(true);
    if let Some(handle) = bridge.take() {
        match tokio::time::timeout(timing.bridge_stop_wait, handle).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(why))) => warn!("the bridge reported a problem while stopping: {why}"),
            Ok(Err(why)) => warn!("the bridge task ended badly: {why}"),
            Err(_) => warn!("the bridge did not close its ports in time; going on without it"),
        }
    }
    if let Some(handle) = api.take() {
        let _ = handle.await;
    }

    // 2. Store everything the bridge already reported.
    let _ = store_stop.send(true);
    if let Err(why) = consumer.await {
        warn!("the share writer ended badly: {why}");
    }

    // 3. Stop watching and paying.
    let _ = back_stop.send(true);
    for (name, handle) in [("reward watcher", watcher), ("reward splitter", allocator), ("payout scheduler", scheduler)] {
        if let Err(why) = handle.await {
            warn!("the {name} ended badly: {why}");
        }
    }
    outcome
}

/// Build the real parts for this binary's chain and run the pool.
pub async fn serve_command<C: Chain + 'static>(args: ServeArgs, secret: Option<String>) -> Result<String, String> {
    let config = PoolConfig::load::<C>(&args.config).map_err(|e| e.to_string())?;
    for note in &config.warnings {
        warn!("{note}");
    }
    let secret = secret
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("The wallet password is missing. Put it in the environment variable {SECRET_VARIABLE} (the service reads it from its environment file)."))?;

    std::fs::create_dir_all(&config.data_dir)
        .map_err(|e| format!("Could not make the data directory {}: {e}", config.data_dir.display()))?;
    let db = Db::open(&config.data_dir.join(DB_FILENAME)).await.map_err(|e| format!("Could not open the pool's database: {e}"))?;

    let node = C::connect_node(&config.node_grpc).await.map_err(|e| e.to_string())?;
    let payer = C::open_payer(&WalletSettings {
        network: config.network.clone(),
        wrpc_url: config.node_wrpc.clone(),
        data_dir: config.data_dir.clone(),
        secret: WalletSecret::new(secret),
    })
    .await
    .map_err(|e| e.to_string())?;
    let launcher = C::bridge_launcher(config.bridge_settings());

    info!("marigold-pool {} starting on {}", env!("CARGO_PKG_VERSION"), config.network);
    let result = run_pool::<C>(&config, db.clone(), Parts { node, payer, launcher }, Timing::production(), wait_for_stop()).await;
    db.close().await;
    result.map(|_| "Stopped.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pool_chain::testing::FakeChain;
    use pool_chain::{
        BlockReport, BridgeError, BridgeLauncher, NodeError, NodeUtxo, NodeView, PayError, PayOutput, Payer, ShareReport, SubmitGuard,
        TxSighting,
    };
    use pool_db::{NewPayout, PayoutStatus};
    use pool_domain::{Account, Amount, MinerAddress};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::{Notify, oneshot};

    const COIN: u64 = 100_000_000;
    const POOL: &str = "fake:pool";

    // ---- fakes ---------------------------------------------------------------------

    struct FakeNode {
        tip: u64,
        utxos: Vec<NodeUtxo>,
    }

    #[async_trait::async_trait]
    impl NodeView for FakeNode {
        async fn virtual_daa_score(&self) -> Result<u64, NodeError> {
            Ok(self.tip)
        }
        async fn utxos_at(&self, _: &str) -> Result<Vec<NodeUtxo>, NodeError> {
            Ok(self.utxos.clone())
        }
        async fn balance_at(&self, _: &str) -> Result<u64, NodeError> {
            Ok(self.utxos.iter().map(|u| u.amount).sum())
        }
        async fn block_is_blue(&self, _: &str) -> Result<bool, NodeError> {
            Ok(true)
        }
    }

    struct FakePayer {
        owns: bool,
    }

    #[async_trait::async_trait]
    impl Payer for FakePayer {
        async fn pay(&self, _: &[PayOutput], _: &dyn SubmitGuard) -> Result<String, PayError> {
            Err(PayError("these tests do not pay".into()))
        }
        async fn sighting(&self, _: &str, _: &[String]) -> Result<TxSighting, PayError> {
            Ok(TxSighting::Unknown)
        }
        async fn spendable(&self) -> Result<u64, PayError> {
            Ok(0)
        }
        async fn owns_address(&self, _: &str) -> Result<bool, PayError> {
            Ok(self.owns)
        }
    }

    /// A bridge that reports some shares and a block, says so, and waits to be stopped.
    struct FakeBridge {
        shares: Vec<(&'static str, u64)>, // (address, how many)
        block: bool,
        emitted: Arc<Notify>,
        stopped_cleanly: Arc<AtomicBool>,
        fail_with: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl BridgeLauncher for FakeBridge {
        async fn run(&self, events: Arc<dyn PoolEvents>, mut shutdown: watch::Receiver<bool>) -> Result<(), BridgeError> {
            if let Some(why) = self.fail_with {
                return Err(BridgeError(why.into()));
            }
            let now = now_ms();
            for (address, count) in &self.shares {
                for i in 0..*count {
                    events.share_accepted(ShareReport {
                        address: (*address).into(),
                        worker: "rig".into(),
                        difficulty: 4.0,
                        hash_value: 1000.0,
                        daa_score: 49_900 + (i % 50),
                        blue_score: 1,
                        timestamp_ms: now,
                    });
                }
            }
            if self.block {
                events.block_found(BlockReport {
                    address: "fake:alice".into(),
                    worker: "rig".into(),
                    hash: "feedbeef".into(),
                    blue_score: 7,
                    daa_score: 49_950,
                    nonce: 5,
                    timestamp_ms: now,
                });
            }
            self.emitted.notify_one();
            let _ = shutdown.wait_for(|stop| *stop).await;
            self.stopped_cleanly.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn config(api_port: u16, window_daa: u64) -> PoolConfig {
        PoolConfig::parse::<FakeChain>(&format!(
            r#"
[chain]
name = "marigold"
[network]
id = "fakenet"
[api]
bind = "127.0.0.1:{api_port}"
[pool]
address = "{POOL}"
window_daa = {window_daa}
data_dir = "/unused"
[stratum]
[[stratum.instances]]
port = ":5570"
min_share_diff = 0.002
"#
        ))
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn fast() -> Timing {
        Timing {
            flush_interval: Duration::from_millis(10),
            prune_interval: Duration::from_secs(3600),
            reward_poll: Duration::from_millis(20),
            allocate_interval: Duration::from_millis(20),
            bridge_stop_wait: Duration::from_secs(5),
        }
    }

    fn who(name: &str) -> MinerAddress {
        MinerAddress::from_trusted(format!("fake:{name}"))
    }

    fn bridge(shares: Vec<(&'static str, u64)>, block: bool) -> (Arc<FakeBridge>, Arc<Notify>, Arc<AtomicBool>) {
        let emitted = Arc::new(Notify::new());
        let stopped = Arc::new(AtomicBool::new(false));
        (
            Arc::new(FakeBridge { shares, block, emitted: emitted.clone(), stopped_cleanly: stopped.clone(), fail_with: None }),
            emitted,
            stopped,
        )
    }

    fn parts(node: FakeNode, owns: bool, launcher: Arc<FakeBridge>) -> Parts {
        Parts { node: Arc::new(node), payer: Arc::new(FakePayer { owns }), launcher }
    }

    async fn get(port: u16, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        reply
    }

    // ---- the whole pipeline ----------------------------------------------------------

    #[tokio::test]
    async fn shares_reach_the_database_and_a_matured_reward_is_split_among_the_miners_by_their_work() {
        let db = Db::open_in_memory().await.unwrap();
        let (launcher, emitted, stopped) = bridge(vec![("fake:alice", 20), ("fake:bob", 10)], true);
        // The reward arrived long ago (tip 100000, reward at 50000) so it has matured; the shares sit in its window.
        let node = FakeNode {
            tip: 100_000,
            utxos: vec![NodeUtxo { outpoint: "r1:0".into(), amount: 10 * COIN, block_daa_score: 50_000, is_coinbase: true }],
        };
        let port = free_port();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();

        let pool = tokio::spawn({
            let db = db.clone();
            async move {
                run_pool::<FakeChain>(&config(port, 1_000), db, parts(node, true, launcher), fast(), async {
                    let _ = stop_rx.await;
                })
                .await
            }
        });

        emitted.notified().await;
        // Wait for the reward to be found and split.
        let mut split = false;
        for _ in 0..500 {
            if db.unallocated_rewards().await.unwrap().is_empty() && db.get_reward("r1:0").await.unwrap().is_some() {
                split = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(split, "the reward was found and split");

        // The pages answer while it runs.
        let reply = get(port, "/api/pool").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.contains("\"miners_online\":2") && reply.contains("\"shares_10m\":30"), "{reply}");

        stop_tx.send(()).unwrap();
        pool.await.unwrap().unwrap();
        assert!(stopped.load(Ordering::SeqCst), "the bridge was told to close its ports");

        assert_eq!(db.count_shares().await.unwrap(), 30);
        assert_eq!(db.count_miners().await.unwrap(), 2);
        let block = db.get_block("feedbeef").await.unwrap().expect("the block was recorded");
        assert_eq!((block.blue_score, block.nonce), (7, 5));
        // 10.00 reward, 1% fee = 0.10, the rest 9.90 split 2:1 by work (20 shares against 10 at the same difficulty).
        let balance = |name: &str| {
            let db = db.clone();
            let account = Account::Miner(who(name));
            async move { db.balance_of(&account).await.unwrap().map(|b| b.balance.units()).unwrap_or(0) }
        };
        assert_eq!(balance("alice").await, 660_000_000);
        assert_eq!(balance("bob").await, 330_000_000);
        assert_eq!(db.balance_of(&Account::Pool).await.unwrap().unwrap().balance, Amount::new(10_000_000));
        assert_eq!(
            db.allocations_for_reward("r1:0").await.unwrap().iter().map(|(_, n)| n.units()).sum::<u64>(),
            10 * COIN,
            "the pieces add up to the reward"
        );
    }

    #[tokio::test]
    async fn stopping_stores_every_share_the_bridge_had_already_reported() {
        let db = Db::open_in_memory().await.unwrap();
        let (launcher, emitted, _) = bridge(vec![("fake:alice", 3000)], false);
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let mut timing = fast();
        timing.flush_interval = Duration::from_secs(3600); // only the stop can write them
        let pool = tokio::spawn({
            let db = db.clone();
            async move {
                run_pool::<FakeChain>(
                    &config(free_port(), 1_000),
                    db,
                    parts(FakeNode { tip: 0, utxos: vec![] }, true, launcher),
                    timing,
                    async {
                        let _ = stop_rx.await;
                    },
                )
                .await
            }
        });
        emitted.notified().await;
        stop_tx.send(()).unwrap();
        pool.await.unwrap().unwrap();
        assert_eq!(db.count_shares().await.unwrap(), 3000, "nothing the bridge reported was lost on the way out");
    }

    // ---- refusals --------------------------------------------------------------------

    #[tokio::test]
    async fn a_wallet_that_does_not_own_the_pool_address_stops_the_pool_before_anything_starts() {
        let db = Db::open_in_memory().await.unwrap();
        let (launcher, _, stopped) = bridge(vec![], false);
        let error = run_pool::<FakeChain>(
            &config(free_port(), 1_000),
            db,
            parts(FakeNode { tip: 0, utxos: vec![] }, false, launcher),
            fast(),
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("does not belong to the pool wallet") && error.contains("fake:pool"), "{error}");
        assert!(!stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_port_that_is_taken_is_reported_before_anything_starts() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let db = Db::open_in_memory().await.unwrap();
        let (launcher, _, _) = bridge(vec![], false);
        let error = run_pool::<FakeChain>(
            &config(port, 1_000),
            db,
            parts(FakeNode { tip: 0, utxos: vec![] }, true, launcher),
            fast(),
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("Could not listen for the pool's pages on"), "{error}");
    }

    #[tokio::test]
    async fn a_bridge_that_cannot_run_takes_the_whole_pool_down_with_its_reason() {
        let db = Db::open_in_memory().await.unwrap();
        let launcher = Arc::new(FakeBridge {
            shares: vec![],
            block: false,
            emitted: Arc::new(Notify::new()),
            stopped_cleanly: Arc::new(AtomicBool::new(false)),
            fail_with: Some("the node is gone"),
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run_pool::<FakeChain>(
                &config(free_port(), 1_000),
                db,
                parts(FakeNode { tip: 0, utxos: vec![] }, true, launcher),
                fast(),
                std::future::pending(),
            ),
        )
        .await
        .expect("the pool stopped instead of running on without its bridge");
        assert_eq!(result.unwrap_err(), "the node is gone");
    }

    // ---- start-up recovery -----------------------------------------------------------

    #[tokio::test]
    async fn at_start_a_payout_that_was_written_down_but_never_sent_is_cancelled_and_the_money_returned() {
        let db = Db::open_in_memory().await.unwrap();
        db.record_reward_if_new("r:0", Amount::new(20 * COIN), 1, 0).await.unwrap();
        db.apply_allocation("r:0", &[(Account::Miner(who("alice")), Amount::new(20 * COIN))], 1).await.unwrap();
        db.create_pending_payouts(
            &[NewPayout { idempotency_key: "payout:alice:1".into(), address: who("alice"), amount: Amount::new(20 * COIN) }],
            5,
        )
        .await
        .unwrap();
        assert_eq!(db.balance_of(&Account::Miner(who("alice"))).await.unwrap().unwrap().balance, Amount::ZERO);

        let (launcher, emitted, _) = bridge(vec![], false);
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let pool = tokio::spawn({
            let db = db.clone();
            async move {
                run_pool::<FakeChain>(
                    &config(free_port(), 1_000),
                    db,
                    parts(FakeNode { tip: 0, utxos: vec![] }, true, launcher),
                    fast(),
                    async {
                        let _ = stop_rx.await;
                    },
                )
                .await
            }
        });
        emitted.notified().await;
        stop_tx.send(()).unwrap();
        pool.await.unwrap().unwrap();

        assert_eq!(db.payouts_with_status(PayoutStatus::Failed).await.unwrap().len(), 1);
        assert_eq!(
            db.balance_of(&Account::Miner(who("alice"))).await.unwrap().unwrap().balance,
            Amount::new(20 * COIN),
            "the money is back where it was"
        );
    }
}
