use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use pool_chain::testing::FakeChain;
use pool_chain::{MarigoldChain, NodeError, NodeUtxo, NodeView};
use pool_db::{NewBlock, NewPayout, NewShare};
use pool_domain::{Account, MinerAddress, ShareWeight, WorkerName};
use std::sync::atomic::Ordering;
use tower::ServiceExt;

/// The stopped clock every test runs on.
const NOW: u64 = 1_000_000_000_000;
const COIN: u64 = 100_000_000;

fn who(name: &str) -> MinerAddress {
    MinerAddress::from_trusted(format!("fake:{name}"))
}

struct UpNode;
struct DownNode;

#[async_trait::async_trait]
impl NodeView for UpNode {
    async fn virtual_daa_score(&self) -> Result<u64, NodeError> {
        Ok(5)
    }
    async fn utxos_at(&self, _: &str) -> Result<Vec<NodeUtxo>, NodeError> {
        Ok(vec![])
    }
    async fn balance_at(&self, _: &str) -> Result<u64, NodeError> {
        Ok(0)
    }
    async fn block_is_blue(&self, _: &str) -> Result<bool, NodeError> {
        Ok(true)
    }
}

#[async_trait::async_trait]
impl NodeView for DownNode {
    async fn virtual_daa_score(&self) -> Result<u64, NodeError> {
        Err(NodeError("down".into()))
    }
    async fn utxos_at(&self, _: &str) -> Result<Vec<NodeUtxo>, NodeError> {
        Err(NodeError("down".into()))
    }
    async fn balance_at(&self, _: &str) -> Result<u64, NodeError> {
        Err(NodeError("down".into()))
    }
    async fn block_is_blue(&self, _: &str) -> Result<bool, NodeError> {
        Err(NodeError("down".into()))
    }
}

fn state(db: Db, network: &str, node: Option<SharedNode>) -> Arc<ApiState> {
    Arc::new(ApiState {
        db,
        network: network.into(),
        pool_address: "fake:pool".into(),
        fee_bp: 100,
        min_payout: Amount::new(10 * COIN),
        window_daa: 36_000,
        metrics: Arc::new(AccountantMetrics::default()),
        node,
        clock: Arc::new(|| NOW),
        payout_schedule: pool_payout::Schedule::parse("0 5 * * *").unwrap(),
        password_work: password_gate(),
    })
}

async fn share(db: &Db, who_: &str, worker: &str, age_ms: u64, hash_value: f64) {
    db.insert_share(&NewShare {
        address: who(who_),
        worker: WorkerName::new(worker),
        weight: ShareWeight::new(1_000),
        hash_value,
        daa_score: 1,
        ts_ms: NOW - age_ms,
    })
    .await
    .unwrap();
}

async fn block(db: &Db, hash: &str, who_: &str, worker: &str, age_ms: u64, blue: Option<bool>) {
    db.insert_block(&NewBlock {
        hash: hash.into(),
        address: who(who_),
        worker: WorkerName::new(worker),
        blue_score: 7,
        daa_score: 8,
        nonce: 9,
        ts_ms: NOW - age_ms,
    })
    .await
    .unwrap();
    if let Some(blue) = blue {
        db.set_block_blue(hash, blue).await.unwrap();
    }
}

/// A pool with two miners, three machines, three blocks, one split reward and one paid payout.
///
/// alice: rig1 (two shares, 6000 + 4000 hashes) and rig2 (one share, 5000 hashes), all within ten minutes.
/// bob: one machine, last seen an hour ago.
/// The one reward of 10.00 is split pool 0.10, alice 6.00, bob 3.90, and bob has been paid his 3.90.
async fn seeded() -> Arc<ApiState> {
    let db = Db::open_in_memory().await.unwrap();
    share(&db, "alice", "rig1", 30_000, 6_000.0).await;
    share(&db, "alice", "rig1", 20_000, 4_000.0).await;
    share(&db, "alice", "rig2", 1_000, 5_000.0).await;
    share(&db, "bob", "x", 3_600_000, 2_000.0).await;
    block(&db, "b1", "alice", "rig1", 60_000, Some(true)).await;
    block(&db, "b2", "bob", "x", 3_600_000, Some(false)).await;
    block(&db, "b3", "alice", "rig2", 5_000, None).await;
    db.record_reward_if_new("r1:0", Amount::new(10 * COIN), 1, 0).await.unwrap();
    db.apply_allocation(
        "r1:0",
        &[
            (Account::Pool, Amount::new(COIN / 10)),
            (Account::Miner(who("alice")), Amount::new(6 * COIN)),
            (Account::Miner(who("bob")), Amount::new(390_000_000)),
        ],
        1,
    )
    .await
    .unwrap();
    let rows = db
        .create_pending_payouts(
            &[NewPayout { idempotency_key: "payout:bob:1".into(), address: who("bob"), amount: Amount::new(390_000_000) }],
            NOW - 1_000,
        )
        .await
        .unwrap();
    db.mark_payouts_sent(&[rows[0].id], "txbob", Some("fake:change"), NOW - 900).await.unwrap();
    db.confirm_payouts(&[rows[0].id], NOW - 800).await.unwrap();
    state(db, "fakenet", Some(Arc::new(UpNode)))
}

async fn request(app: &Router, path: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec();
    (status, headers, body)
}

async fn json(app: &Router, path: &str) -> (StatusCode, serde_json::Value) {
    let (status, _, body) = request(app, path).await;
    (status, serde_json::from_slice(&body).unwrap_or_else(|e| panic!("{path}: not JSON ({e}): {}", String::from_utf8_lossy(&body))))
}

fn fake_app(state: Arc<ApiState>) -> Router {
    router::<FakeChain>(state)
}

// ---- the pool -----------------------------------------------------------------

#[tokio::test]
async fn the_pool_page_returns_the_seeded_numbers() {
    let app = fake_app(seeded().await);
    let (status, body) = json(&app, "/api/pool").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["network"], "fakenet");
    assert_eq!(body["ticker"], "TST");
    assert_eq!(body["now_ms"], NOW);
    assert_eq!(body["miners_online"], 1, "bob was last seen an hour ago");
    assert_eq!(body["workers_online"], 2);
    assert_eq!(body["shares_10m"], 3);
    assert_eq!(body["hashrate_hps"], 25.0, "15000 hashes over 600 seconds");
    assert_eq!(body["blocks_24h"], 3);
    assert_eq!(body["last_block_ms"], NOW - 5_000);
    assert_eq!(body["fee_bp"], 100);
    assert_eq!(body["fee_percent"], "1.00");
    assert_eq!(body["min_payout"]["display"], "10.00 TST");
    assert_eq!(body["min_payout"]["units"], 10 * COIN);
    assert_eq!(body["window_daa"], 36_000);
    assert_eq!(body["owed_to_miners"]["display"], "6.00 TST", "bob was paid, only alice is owed");
    assert_eq!(body["pool_address"], "fake:pool");
}

#[tokio::test]
async fn an_empty_pool_answers_with_zeroes_and_no_last_block() {
    let app = fake_app(state(Db::open_in_memory().await.unwrap(), "fakenet", None));
    let (status, body) = json(&app, "/api/pool").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (body["miners_online"].clone(), body["hashrate_hps"].clone(), body["last_block_ms"].clone()),
        (0.into(), 0.0.into(), serde_json::Value::Null)
    );
}

#[tokio::test]
async fn the_fee_reads_as_a_percentage() {
    for (bp, text) in [(0u16, "0.00"), (100, "1.00"), (150, "1.50"), (5, "0.05"), (10_000, "100.00")] {
        let mut s = seeded().await;
        Arc::get_mut(&mut s).unwrap().fee_bp = bp;
        let (_, body) = json(&fake_app(s), "/api/pool").await;
        assert_eq!(body["fee_percent"], text, "{bp}");
    }
}

// ---- one miner ------------------------------------------------------------------

#[tokio::test]
async fn a_miners_page_has_their_money_machines_and_payments() {
    let app = fake_app(seeded().await);
    let (status, body) = json(&app, "/api/miner/fake:alice").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["address"], "fake:alice");
    assert_eq!(body["balance"]["display"], "6.00 TST");
    assert_eq!(body["total_earned"]["units"], 6 * COIN);
    assert_eq!(body["total_paid"]["units"], 0);
    let workers = body["workers"].as_array().unwrap();
    assert_eq!(
        workers.iter().map(|w| w["name"].as_str().unwrap()).collect::<Vec<_>>(),
        ["rig2", "rig1"],
        "most recently active first"
    );
    assert_eq!(workers[0]["seconds_since_last_share"], 1);
    assert!((workers[0]["hashrate_hps"].as_f64().unwrap() - 5_000.0 / 600.0).abs() < 1e-9);
    assert!((workers[1]["hashrate_hps"].as_f64().unwrap() - 10_000.0 / 600.0).abs() < 1e-9);
    assert!(body["payouts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_miner_who_was_paid_sees_the_payment_in_words() {
    let app = fake_app(seeded().await);
    let (_, body) = json(&app, "/api/miner/fake:bob").await;
    assert_eq!(body["balance"]["units"], 0);
    assert_eq!(body["total_paid"]["display"], "3.90 TST");
    let payments = body["payouts"].as_array().unwrap();
    assert_eq!(payments.len(), 1);
    assert_eq!(
        (payments[0]["status"].clone(), payments[0]["status_text"].clone(), payments[0]["txid"].clone()),
        ("confirmed".into(), "paid".into(), "txbob".into())
    );
    assert_eq!(payments[0]["amount"]["display"], "3.90 TST");
    let workers = body["workers"].as_array().unwrap();
    assert_eq!(
        (workers[0]["hashrate_hps"].clone(), workers[0]["seconds_since_last_share"].clone()),
        (0.0.into(), 3600.into()),
        "seen an hour ago, so no speed now"
    );
}

#[tokio::test]
async fn an_address_that_is_not_one_gets_the_chains_own_sentence() {
    let app = fake_app(seeded().await);
    let (status, body) = json(&app, "/api/miner/nonsense").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "That does not look like an address. It starts with fake:.");
}

#[tokio::test]
async fn on_marigold_the_bad_address_sentence_names_the_prefix_and_the_network() {
    let app = router::<MarigoldChain>(state(Db::open_in_memory().await.unwrap(), "testnet-10", None));
    let (status, body) = json(&app, "/api/miner/hello").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "That does not look like a Marigold address. It starts with marigoldtest: on the test network.");
}

#[tokio::test]
async fn a_good_address_the_pool_has_never_seen_is_told_to_wait() {
    let app = fake_app(seeded().await);
    let (status, body) = json(&app, "/api/miner/fake:carol").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body["error"],
        "The pool has not seen any work or earnings from this address yet. If you have only just started mining, wait a minute and try again."
    );
}

#[tokio::test]
async fn a_miner_with_only_a_payment_history_is_still_found() {
    let db = Db::open_in_memory().await.unwrap();
    db.record_reward_if_new("r:0", Amount::new(20 * COIN), 1, 0).await.unwrap();
    db.apply_allocation("r:0", &[(Account::Miner(who("dave")), Amount::new(20 * COIN))], 1).await.unwrap();
    let app = fake_app(state(db, "fakenet", None));
    let (status, body) = json(&app, "/api/miner/fake:dave").await;
    assert_eq!(status, StatusCode::OK, "earnings alone are enough");
    assert_eq!(body["balance"]["display"], "20.00 TST");
    assert!(body["workers"].as_array().unwrap().is_empty());
}

// ---- lists ------------------------------------------------------------------------

#[tokio::test]
async fn blocks_come_newest_first_with_their_result_in_words() {
    let app = fake_app(seeded().await);
    let (status, body) = json(&app, "/api/blocks").await;
    assert_eq!(status, StatusCode::OK);
    let list = body.as_array().unwrap();
    assert_eq!(list.iter().map(|b| b["hash"].as_str().unwrap()).collect::<Vec<_>>(), ["b3", "b1", "b2"]);
    assert_eq!(list.iter().map(|b| b["status"].as_str().unwrap()).collect::<Vec<_>>(), ["checking", "counted", "not counted"]);
    assert_eq!(list[0]["address"], "fake:alice");
    assert_eq!(list[0]["time_ms"], NOW - 5_000);
}

#[tokio::test]
async fn the_limit_is_honoured_clamped_and_checked() {
    let app = fake_app(seeded().await);
    assert_eq!(json(&app, "/api/blocks?limit=2").await.1.as_array().unwrap().len(), 2);
    assert_eq!(json(&app, "/api/blocks?limit=1").await.1.as_array().unwrap().len(), 1);
    assert_eq!(json(&app, "/api/blocks?limit=100000").await.1.as_array().unwrap().len(), 3, "too big counts as the most allowed");
    for bad in ["0", "-1", "abc", "1.5", ""] {
        let (status, body) = json(&app, &format!("/api/blocks?limit={bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "limit={bad:?}");
        assert_eq!(body["error"], "The limit must be a whole number from 1 to 100.");
    }
}

#[tokio::test]
async fn payments_list_newest_first_in_words() {
    let app = fake_app(seeded().await);
    let (status, body) = json(&app, "/api/payouts?limit=5").await;
    assert_eq!(status, StatusCode::OK);
    let list = body.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0]["address"].clone(), list[0]["status_text"].clone(), list[0]["amount"]["display"].clone()),
        ("fake:bob".into(), "paid".into(), "3.90 TST".into())
    );
}

// ---- health -------------------------------------------------------------------------

#[tokio::test]
async fn health_is_ok_when_the_database_and_the_node_answer() {
    let app = fake_app(seeded().await);
    let (status, body) = json(&app, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((body["ok"].clone(), body["database"].clone(), body["node"].clone()), (true.into(), "ok".into(), "reachable".into()));
    assert_eq!(body["last_share_age_seconds"], 1);
}

#[tokio::test]
async fn health_says_so_when_the_node_is_down() {
    let app = fake_app(state(Db::open_in_memory().await.unwrap(), "fakenet", Some(Arc::new(DownNode))));
    let (status, body) = json(&app, "/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!((body["ok"].clone(), body["node"].clone()), (false.into(), "unreachable".into()));
    assert_eq!(body["last_share_age_seconds"], serde_json::Value::Null, "no shares yet");
}

#[tokio::test]
async fn without_a_node_to_ask_health_only_vouches_for_the_database() {
    let app = fake_app(state(Db::open_in_memory().await.unwrap(), "fakenet", None));
    let (status, body) = json(&app, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["node"], "not checked");
}

// ---- metrics ------------------------------------------------------------------------

fn sample(scrape: &prometheus_parse::Scrape, name: &str, label: Option<(&str, &str)>) -> f64 {
    use prometheus_parse::Value;
    let found = scrape
        .samples
        .iter()
        .find(|s| s.metric == name && label.is_none_or(|(k, v)| s.labels.get(k) == Some(v)))
        .unwrap_or_else(|| panic!("no sample named {name} {label:?}"));
    match found.value {
        Value::Counter(v) | Value::Gauge(v) | Value::Untyped(v) => v,
        _ => panic!("{name} is not a plain number"),
    }
}

#[tokio::test]
async fn the_metrics_parse_as_prometheus_text_and_carry_the_numbers() {
    let s = seeded().await;
    s.metrics.shares_stored.store(1234, Ordering::Relaxed);
    s.metrics.blocks_stored.store(7, Ordering::Relaxed);
    s.metrics.rewards_seen.store(5, Ordering::Relaxed);
    s.metrics.rewards_allocated.store(4, Ordering::Relaxed);
    s.metrics.node_errors.store(2, Ordering::Relaxed);
    let app = fake_app(s);

    let (status, headers, body) = request(&app, "/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/plain"), "{headers:?}");
    let text = String::from_utf8(body).unwrap();
    let scrape = prometheus_parse::Scrape::parse(text.lines().map(|l| Ok(l.to_owned()))).expect("valid Prometheus text");

    assert_eq!(sample(&scrape, "marigold_pool_shares_stored_total", None), 1234.0);
    assert_eq!(sample(&scrape, "marigold_pool_blocks_found_total", None), 7.0);
    assert_eq!(sample(&scrape, "marigold_pool_rewards_seen_total", None), 5.0);
    assert_eq!(sample(&scrape, "marigold_pool_rewards_allocated_total", None), 4.0);
    assert_eq!(sample(&scrape, "marigold_pool_node_errors_total", None), 2.0);
    assert_eq!(sample(&scrape, "marigold_pool_miners_online", None), 1.0);
    assert_eq!(sample(&scrape, "marigold_pool_workers_online", None), 2.0);
    assert_eq!(sample(&scrape, "marigold_pool_blocks_24h", None), 3.0);
    assert_eq!(sample(&scrape, "marigold_pool_owed_units", None), (6 * COIN) as f64);
    assert_eq!(sample(&scrape, "marigold_pool_hashrate_hps", None), 25.0);
    assert_eq!(sample(&scrape, "marigold_pool_payouts", Some(("status", "confirmed"))), 1.0);
    assert_eq!(sample(&scrape, "marigold_pool_payouts", Some(("status", "failed"))), 0.0);
}

// ---- the page ---------------------------------------------------------------------------

#[tokio::test]
async fn the_front_page_is_html_with_a_strict_content_policy() {
    let app = fake_app(seeded().await);
    let (status, headers, body) = request(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
    let policy = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(
        policy.contains("default-src 'none'") && policy.contains("connect-src 'self'") && policy.contains("frame-ancestors 'none'"),
        "{policy}"
    );
    let page = String::from_utf8(body).unwrap();
    assert!(page.contains("<title>Marigold mining pool</title>"));
    assert!(page.contains("Show my numbers"));
}

#[tokio::test]
async fn nothing_is_cached_and_nothing_is_guessed_at() {
    let app = fake_app(seeded().await);
    for path in ["/", "/api/pool", "/health", "/metrics", "/api/blocks"] {
        let (_, headers, _) = request(&app, path).await;
        assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{path}");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
    }
}

#[test]
fn the_page_never_puts_text_from_the_pool_into_the_page_as_markup() {
    let page = include_str!("dashboard.html");
    for forbidden in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "eval(", "new Function"] {
        assert!(!page.contains(forbidden), "the page uses {forbidden}");
    }
    assert!(page.contains("textContent"), "values go in as text");
}

#[test]
fn the_page_says_the_same_things_the_api_does_about_amounts_and_states() {
    let page = include_str!("dashboard.html");
    // It shows the strings the server made (two decimals, the ticker) rather than formatting money itself.
    assert!(page.contains(".display"));
    assert!(page.contains("status_text"));
    assert!(!page.contains("toFixed(8)"), "no money arithmetic in the browser");
}

// ---- the server ----------------------------------------------------------------------------

#[tokio::test]
async fn the_server_answers_over_a_socket_and_stops_when_asked() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, shutdown) = watch::channel(false);
    let server = tokio::spawn(serve::<FakeChain>(seeded().await, listener, shutdown));

    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(b"GET /api/pool HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.contains("\"miners_online\":1"), "{reply}");

    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), server).await.expect("the server stopped").unwrap().unwrap();
}

// ---- payment rules in words ----------------------------------------------------------

async fn post_json(app: &Router, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{path}: not JSON ({e}): {}", String::from_utf8_lossy(&bytes))))
}

#[tokio::test]
async fn the_pool_says_when_it_pays_in_utc_and_what_the_rule_is() {
    let app = fake_app(seeded().await);
    let (_, body) = json(&app, "/api/pool").await;
    assert_eq!(body["payout_schedule"], "every day at 05:00 UTC");
    assert_eq!(body["next_payout_ms"], 1_000_011_600_000u64, "05:00 UTC on the same day as the stopped clock");
    assert_eq!(
        body["payout_rule"],
        "Payments are sent every day at 05:00 UTC to every miner who is owed at least their payout level. The level starts at 10.00 TST and each miner can choose a higher one."
    );
}

#[tokio::test]
async fn a_miner_below_their_level_is_told_what_they_are_waiting_for() {
    let app = fake_app(seeded().await);
    let (_, alice) = json(&app, "/api/miner/fake:alice").await;
    assert_eq!(
        alice["payout_note"],
        "You are owed 6.00 TST so far. You are paid once you are owed at least 10.00 TST, your payout level, at the first payment run after that. Payments are sent every day at 05:00 UTC."
    );
    assert_eq!(
        (alice["payout_level"]["display"].clone(), alice["payout_level_chosen"].clone(), alice["password_set"].clone()),
        ("10.00 TST".into(), false.into(), false.into())
    );
    assert_eq!(
        (alice["lowest_payout_level"]["display"].clone(), alice["highest_payout_level"]["display"].clone()),
        ("10.00 TST".into(), "1000000.00 TST".into())
    );
}

#[tokio::test]
async fn a_miner_owed_enough_is_told_when_the_next_payment_is() {
    let s = seeded().await;
    s.db.record_reward_if_new("r2:0", Amount::new(20 * COIN), 2, 0).await.unwrap();
    s.db.apply_allocation("r2:0", &[(Account::Miner(who("dave")), Amount::new(20 * COIN))], 2).await.unwrap();
    let app = fake_app(s);
    let (_, dave) = json(&app, "/api/miner/fake:dave").await;
    // The stopped clock is 01:46:40 UTC; the next run is at 05:00, three hours and thirteen minutes away.
    assert_eq!(
        dave["payout_note"],
        "You are owed 20.00 TST, which is enough. You will be paid at the next payment run, every day at 05:00 UTC, in about 4 hours."
    );
}

#[tokio::test]
async fn a_miner_with_nothing_yet_and_one_who_is_paid_up_get_their_own_words() {
    let s = seeded().await;
    share(&s.db, "carol", "x", 1_000, 1.0).await;
    let app = fake_app(s);
    let (_, carol) = json(&app, "/api/miner/fake:carol").await;
    assert_eq!(carol["payout_note"], "You have not earned anything yet. Rewards are shared out when the pool finds a block.");
    let (_, bob) = json(&app, "/api/miner/fake:bob").await;
    assert_eq!(bob["payout_note"], "Everything you have earned has been paid.");
}

#[test]
fn lengths_of_time_are_said_the_way_a_person_says_them() {
    use crate::handlers::in_words_for_tests as words;
    assert_eq!(words(0), "a minute");
    assert_eq!(words(60_000), "a minute");
    assert_eq!(words(61_000), "2 minutes");
    assert_eq!(words(89 * 60_000), "89 minutes");
    assert_eq!(words(90 * 60_000), "2 hours");
    assert_eq!(words(47 * 3_600_000), "47 hours");
    assert_eq!(words(49 * 3_600_000), "3 days");
}

// ---- choosing a password -------------------------------------------------------------

const PASSWORD: &str = "correct horse battery";

#[tokio::test]
async fn a_password_can_be_chosen_once_for_an_address_the_pool_has_seen() {
    let s = seeded().await;
    let app = fake_app(s.clone());
    let (status, body) = post_json(&app, "/api/miner/fake:alice/password", serde_json::json!({ "password": PASSWORD })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["message"],
        "Your password is set. You will need it every time you change a setting, so keep it safe. The pool cannot show it to you again."
    );

    let stored = s.db.miner_account(&who("alice")).await.unwrap().unwrap();
    assert!(stored.password_hash.starts_with("$argon2"), "only a hash is kept");
    assert!(!stored.password_hash.contains(PASSWORD));
    let (_, alice) = json(&app, "/api/miner/fake:alice").await;
    assert_eq!(alice["password_set"], true);

    let (status, again) =
        post_json(&app, "/api/miner/fake:alice/password", serde_json::json!({ "password": "another password" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(again["error"], "This address already has a password. Enter it to change your settings.");
    assert!(verify_still(&s, "alice", PASSWORD).await, "the first password still stands: nobody can take over by asking again");
}

async fn verify_still(s: &Arc<ApiState>, name: &str, password: &str) -> bool {
    let account = s.db.miner_account(&who(name)).await.unwrap().unwrap();
    crate::passwords::verify_password(password, &account.password_hash)
}

#[tokio::test]
async fn an_address_the_pool_has_never_seen_cannot_be_claimed_in_advance() {
    let app = fake_app(seeded().await);
    let (status, body) = post_json(&app, "/api/miner/fake:zed/password", serde_json::json!({ "password": PASSWORD })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body["error"],
        "The pool has not seen any work or earnings from this address yet, so a password cannot be set for it. Start mining with it, wait a minute, then try again."
    );
}

#[tokio::test]
async fn a_password_that_is_too_short_or_an_address_that_is_not_one_is_refused_in_words() {
    let app = fake_app(seeded().await);
    let (status, body) = post_json(&app, "/api/miner/fake:alice/password", serde_json::json!({ "password": "short" })).await;
    assert_eq!((status, body["error"].clone()), (StatusCode::BAD_REQUEST, "Choose a password of at least 8 characters.".into()));
    let (status, body) = post_json(&app, "/api/miner/nonsense/password", serde_json::json!({ "password": PASSWORD })).await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, "That does not look like an address. It starts with fake:.".into())
    );
}

// ---- using it ------------------------------------------------------------------------

async fn with_password(name: &str) -> (Arc<ApiState>, Router) {
    let s = seeded().await;
    let app = fake_app(s.clone());
    let (status, _) = post_json(&app, &format!("/api/miner/fake:{name}/password"), serde_json::json!({ "password": PASSWORD })).await;
    assert_eq!(status, StatusCode::OK);
    (s, app)
}

#[tokio::test]
async fn the_right_password_changes_the_payout_level_and_it_shows_on_their_page() {
    let (_s, app) = with_password("alice").await;
    let (status, body) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "25" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Your payout level is now 25.00 TST. You will be paid once you are owed at least that.");
    assert_eq!(body["payout_level"]["units"], 25 * COIN);
    let (_, alice) = json(&app, "/api/miner/fake:alice").await;
    assert_eq!((alice["payout_level"]["display"].clone(), alice["payout_level_chosen"].clone()), ("25.00 TST".into(), true.into()));
    assert!(alice["payout_note"].as_str().unwrap().contains("at least 25.00 TST, your payout level"), "{}", alice["payout_note"]);
}

#[tokio::test]
async fn a_level_can_be_written_with_decimals_and_is_read_exactly() {
    let (_s, app) = with_password("alice").await;
    let (_, body) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "12.34567891" }))
            .await;
    assert_eq!(body["payout_level"]["units"], 1_234_567_891u64);
}

#[tokio::test]
async fn a_level_below_the_pools_lowest_does_not_work_and_says_why() {
    let (s, app) = with_password("alice").await;
    for low in ["9.99999999", "5", "0", "0.5"] {
        let (status, body) =
            post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": low })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{low}");
        assert_eq!(
            body["error"],
            "The lowest payout level is 10.00 TST. Anything lower does not work, because small payments cost the network more to send than they are worth.",
            "{low}"
        );
    }
    assert_eq!(s.db.miner_account(&who("alice")).await.unwrap().unwrap().payout_level, None, "nothing was changed");
    let (status, _) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "10" })).await;
    assert_eq!(status, StatusCode::OK, "ten itself is allowed");
}

#[tokio::test]
async fn nonsense_and_absurd_levels_are_refused() {
    let (_s, app) = with_password("alice").await;
    for bad in ["abc", "", "-20", "1e3", "12,5", "1.123456789"] {
        let (status, body) =
            post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": bad })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
        assert_eq!(body["error"], "That is not a number of coins. Write it like 25 or 12.5.", "{bad:?}");
    }
    let (status, body) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "1000001" }))
            .await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, "The highest payout level you can choose is 1000000.00 TST.".into())
    );
}

#[tokio::test]
async fn asking_to_change_nothing_is_told_so() {
    let (_s, app) = with_password("alice").await;
    let (status, body) = post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD })).await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::BAD_REQUEST, "There is nothing to change. Enter a new payout level, a new password, or both.".into())
    );
}

#[tokio::test]
async fn the_wrong_password_changes_nothing() {
    let (s, app) = with_password("alice").await;
    let (status, body) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": "not the password", "payout_level": "99" }))
            .await;
    assert_eq!((status, body["error"].clone()), (StatusCode::FORBIDDEN, "That password is not right.".into()));
    let account = s.db.miner_account(&who("alice")).await.unwrap().unwrap();
    assert_eq!((account.payout_level, account.failed_attempts), (None, 1));
}

#[tokio::test]
async fn settings_cannot_be_changed_before_a_password_exists() {
    let app = fake_app(seeded().await);
    let (status, body) =
        post_json(&app, "/api/miner/fake:bob/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "50" })).await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::NOT_FOUND, "No password has been set for this address yet. Set one first.".into())
    );
}

#[tokio::test]
async fn the_password_itself_can_be_changed_and_only_the_new_one_works_afterwards() {
    let (_s, app) = with_password("alice").await;
    let (status, body) = post_json(
        &app,
        "/api/miner/fake:alice/settings",
        serde_json::json!({ "password": PASSWORD, "new_password": "a brand new password" }),
    )
    .await;
    assert_eq!((status, body["message"].clone()), (StatusCode::OK, "Your password has been changed.".into()));
    let (status, _) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "20" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the old one no longer works");
    let (status, _) = post_json(
        &app,
        "/api/miner/fake:alice/settings",
        serde_json::json!({ "password": "a brand new password", "payout_level": "20" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_new_password_that_is_too_short_changes_nothing() {
    let (_s, app) = with_password("alice").await;
    let (status, body) = post_json(
        &app,
        "/api/miner/fake:alice/settings",
        serde_json::json!({ "password": PASSWORD, "new_password": "tiny", "payout_level": "30" }),
    )
    .await;
    assert_eq!((status, body["error"].clone()), (StatusCode::BAD_REQUEST, "Choose a password of at least 8 characters.".into()));
    let (_, alice) = json(&app, "/api/miner/fake:alice").await;
    assert_eq!(alice["payout_level_chosen"], false, "not even the level that came with it");
}

// ---- guessing ------------------------------------------------------------------------

#[tokio::test]
async fn five_wrong_passwords_lock_the_address_for_fifteen_minutes_even_against_the_right_one() {
    let mut s = seeded().await;
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(NOW));
    let c = clock.clone();
    Arc::get_mut(&mut s).unwrap().clock = Arc::new(move || c.load(Ordering::SeqCst));
    let app = fake_app(s.clone());
    post_json(&app, "/api/miner/fake:alice/password", serde_json::json!({ "password": PASSWORD })).await;

    for n in 1..=4 {
        let (status, _) = post_json(
            &app,
            "/api/miner/fake:alice/settings",
            serde_json::json!({ "password": "wrong password", "payout_level": "20" }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "attempt {n}");
    }
    let (status, body) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": "wrong password", "payout_level": "20" }))
            .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body["error"],
        "That password is not right, and there have now been too many wrong ones. This address is locked for 15 minutes."
    );

    // Even the right password is not tried while the lock holds.
    let (status, body) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "20" })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "There have been too many wrong passwords for this address. Try again in 15 minutes.");

    clock.store(NOW + 10 * 60_000, Ordering::SeqCst);
    let (_, body) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "20" })).await;
    assert_eq!(body["error"], "There have been too many wrong passwords for this address. Try again in 5 minutes.");

    clock.store(NOW + 15 * 60_000 + 1, Ordering::SeqCst);
    let (status, _) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "20" })).await;
    assert_eq!(status, StatusCode::OK, "once the lock has run out the right password works");
}

#[tokio::test]
async fn a_right_password_in_between_clears_the_count_of_wrong_ones() {
    let (s, app) = with_password("alice").await;
    for _ in 0..4 {
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": "wrong password", "payout_level": "20" }))
            .await;
    }
    post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "20" })).await;
    assert_eq!(s.db.miner_account(&who("alice")).await.unwrap().unwrap().failed_attempts, 0);
    let (status, _) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": "wrong password", "payout_level": "20" }))
            .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "four wrong, one right, one wrong is not a lock");
}

#[tokio::test]
async fn when_the_pool_is_busy_checking_passwords_it_says_so_and_changes_nothing() {
    let (s, app) = with_password("alice").await;
    let _held = s.password_work.clone().try_acquire_many_owned(2).unwrap();
    let (status, body) =
        post_json(&app, "/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "20" })).await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::SERVICE_UNAVAILABLE, "The pool is busy checking passwords. Please try again in a moment.".into())
    );
    let account = s.db.miner_account(&who("alice")).await.unwrap().unwrap();
    assert_eq!((account.payout_level, account.failed_attempts), (None, 0), "being busy is not a wrong password");
}

#[tokio::test]
async fn requests_that_are_too_big_or_not_json_are_refused_in_words() {
    let app = fake_app(seeded().await);
    let big = serde_json::json!({ "password": "x".repeat(5000) });
    let (status, body) = post_json(&app, "/api/miner/fake:alice/password", big).await;
    assert_eq!((status, body["error"].clone()), (StatusCode::PAYLOAD_TOO_LARGE, "That request is too big.".into()));

    let plain = Request::builder()
        .method("POST")
        .uri("/api/miner/fake:alice/password")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("password=x"))
        .unwrap();
    let response = app.clone().oneshot(plain).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "a web form from another site cannot do this");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("That request must be sent as JSON."));

    let broken = Request::builder()
        .method("POST")
        .uri("/api/miner/fake:alice/password")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let response = app.clone().oneshot(broken).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("That request was not understood."));
}

#[tokio::test]
async fn nothing_in_a_reply_ever_contains_a_password() {
    let (_s, app) = with_password("alice").await;
    for (path, body) in [
        ("/api/miner/fake:alice/settings", serde_json::json!({ "password": PASSWORD, "payout_level": "20" })),
        ("/api/miner/fake:alice/settings", serde_json::json!({ "password": "the wrong password here", "payout_level": "20" })),
        ("/api/miner/fake:alice/password", serde_json::json!({ "password": "another one entirely" })),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let bytes = to_bytes(app.clone().oneshot(request).await.unwrap().into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        for secret in [PASSWORD, "the wrong password here", "another one entirely"] {
            assert!(!text.contains(secret), "{path}: {text}");
        }
    }
    let (_, page) = json(&app, "/api/miner/fake:alice").await;
    assert!(!page.to_string().contains("argon2"), "not even the hash is shown");
}

// ---- the page, again --------------------------------------------------------------------

#[test]
fn the_page_asks_for_passwords_in_password_fields_and_warns_when_the_connection_is_not_secure() {
    let page = include_str!("dashboard.html");
    assert!(page.contains("\"password\", \"new-password\"") && page.contains("\"password\", \"current-password\""));
    assert!(page.contains("is not using a secure (https) connection"));
    assert!(page.contains("connectionIsSafe"));
    assert!(page.contains("Protect your settings") && page.contains("Your payout level"));
}

#[test]
fn the_page_never_stores_a_password() {
    let page = include_str!("dashboard.html");
    assert_eq!(page.matches("localStorage.setItem").count(), 1, "only one thing is ever stored");
    assert!(page.contains("localStorage.setItem(\"pool-address\""), "and it is the address, which is public");
    assert!(!page.contains("sessionStorage") && !page.contains("document.cookie"));
}
