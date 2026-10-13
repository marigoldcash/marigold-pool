use crate::ApiState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use pool_chain::Chain;
use pool_db::{BlockRow, DbError, ONLINE_WINDOW_MS, Payout, PayoutStatus};
use pool_domain::{Account, Amount, MinerAddress};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::error;

/// How many rows a list shows when asked for none in particular, and the most it will ever show.
const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;
const MINER_PAYOUTS: u32 = 20;
const NODE_TIMEOUT: Duration = Duration::from_secs(3);

/// What goes wrong, as a status and a sentence a person can read. The page
/// shows the sentence as it is.
pub(crate) struct ApiError(pub(crate) StatusCode, pub(crate) String);

impl ApiError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        ApiError(status, message.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(error: DbError) -> Self {
        error!("the API could not read the database: {error}");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "The pool could not read its records just now. Please try again in a moment.")
    }
}

#[derive(Serialize)]
pub(crate) struct Money {
    pub(crate) units: u64,
    pub(crate) display: String,
}

pub(crate) fn money<C: Chain>(amount: Amount) -> Money {
    Money { units: amount.units(), display: amount.display::<C>().to_string() }
}

/// Expected hashes over the online window, as hashes per second.
fn hashrate(hash_value: f64) -> f64 {
    hash_value / (ONLINE_WINDOW_MS as f64 / 1000.0)
}

/// A length of time as a person would say it: "a minute", "40 minutes", "17 hours".
fn in_words(ms: u64) -> String {
    let minutes = ms.div_ceil(60_000);
    if minutes <= 1 {
        "a minute".to_owned()
    } else if minutes < 90 {
        format!("{minutes} minutes")
    } else if minutes < 48 * 60 {
        format!("{} hours", minutes.div_ceil(60))
    } else {
        format!("{} days", minutes.div_ceil(60 * 24))
    }
}

#[cfg(test)]
pub(crate) fn in_words_for_tests(ms: u64) -> String {
    in_words(ms)
}

fn percent(fee_bp: u16) -> String {
    format!("{}.{:02}", fee_bp / 100, fee_bp % 100)
}

fn payout_words(status: PayoutStatus) -> &'static str {
    match status {
        PayoutStatus::Pending => "being prepared",
        PayoutStatus::Sent => "sent, waiting for the network",
        PayoutStatus::Confirmed => "paid",
        PayoutStatus::Failed => "cancelled, the money went back to the balance",
    }
}

fn block_words(blue: Option<bool>) -> &'static str {
    match blue {
        Some(true) => "counted",
        Some(false) => "not counted",
        None => "checking",
    }
}

// ---- the page ---------------------------------------------------------------

pub(crate) async fn dashboard() -> impl IntoResponse {
    (
        [(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'none'; connect-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            ),
        )],
        Html(include_str!("dashboard.html")),
    )
}

// ---- health -----------------------------------------------------------------

pub(crate) async fn health(State(state): State<Arc<ApiState>>) -> Response {
    let now = (state.clock)();
    let database = state.db.pool_stats(now).await;
    let node = match &state.node {
        None => "not checked",
        Some(node) => match tokio::time::timeout(NODE_TIMEOUT, node.virtual_daa_score()).await {
            Ok(Ok(_)) => "reachable",
            _ => "unreachable",
        },
    };
    let ok = database.is_ok() && node != "unreachable";
    let last_share_age_seconds = database.as_ref().ok().and_then(|s| s.last_share_ms).map(|t| now.saturating_sub(t) / 1000);
    let body = serde_json::json!({
        "ok": ok,
        "database": if database.is_ok() { "ok" } else { "unavailable" },
        "node": node,
        "last_share_age_seconds": last_share_age_seconds,
    });
    ((if ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }), Json(body)).into_response()
}

// ---- the pool ---------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct PoolJson {
    network: String,
    ticker: &'static str,
    now_ms: u64,
    miners_online: u64,
    workers_online: u64,
    hashrate_hps: f64,
    shares_10m: u64,
    blocks_24h: u64,
    last_block_ms: Option<u64>,
    fee_bp: u16,
    fee_percent: String,
    min_payout: Money,
    window_daa: u64,
    owed_to_miners: Money,
    pool_address: String,
    payout_schedule: String,
    next_payout_ms: u64,
    payout_rule: String,
}

pub(crate) async fn pool<C: Chain>(State(state): State<Arc<ApiState>>) -> Result<Json<PoolJson>, ApiError> {
    let now = (state.clock)();
    let stats = state.db.pool_stats(now).await?;
    let schedule = state.payout_schedule.describe();
    Ok(Json(PoolJson {
        network: state.network.clone(),
        ticker: C::ticker(),
        now_ms: now,
        miners_online: stats.miners_online,
        workers_online: stats.workers_online,
        hashrate_hps: hashrate(stats.hash_value_online_window),
        shares_10m: stats.shares_online_window,
        blocks_24h: stats.blocks_24h,
        last_block_ms: stats.last_block_ms,
        fee_bp: state.fee_bp,
        fee_percent: percent(state.fee_bp),
        min_payout: money::<C>(state.min_payout),
        window_daa: state.window_daa,
        owed_to_miners: money::<C>(stats.owed_to_miners),
        pool_address: state.pool_address.clone(),
        next_payout_ms: state.payout_schedule.next_run_ms(now),
        payout_rule: format!(
            "Payments are sent {schedule} to every miner who is owed at least their payout level. The level starts at {} and each miner can choose a higher one.",
            state.min_payout.display::<C>()
        ),
        payout_schedule: schedule,
    }))
}

// ---- one miner ---------------------------------------------------------------

#[derive(Serialize)]
struct WorkerJson {
    name: String,
    hashrate_hps: f64,
    last_share_ms: u64,
    seconds_since_last_share: u64,
}

#[derive(Serialize)]
struct MinerPayoutJson {
    amount: Money,
    status: &'static str,
    status_text: &'static str,
    txid: Option<String>,
    time_ms: u64,
}

#[derive(Serialize)]
pub(crate) struct MinerJson {
    address: String,
    balance: Money,
    total_earned: Money,
    total_paid: Money,
    workers: Vec<WorkerJson>,
    payouts: Vec<MinerPayoutJson>,
    /// Whether this miner has chosen a password, which they need to change anything here.
    password_set: bool,
    /// What they are paid at: the higher of the pool's minimum and the level they chose.
    payout_level: Money,
    /// Whether they have chosen a level of their own.
    payout_level_chosen: bool,
    lowest_payout_level: Money,
    highest_payout_level: Money,
    /// When and why they will or will not be paid, in a sentence.
    payout_note: String,
    next_payout_ms: u64,
}

pub(crate) async fn miner<C: Chain>(
    State(state): State<Arc<ApiState>>,
    Path(address): Path<String>,
) -> Result<Json<MinerJson>, ApiError> {
    let address =
        MinerAddress::parse::<C>(&state.network, &address).map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))?;
    let now = (state.clock)();
    let books = state.db.balance_of(&Account::Miner(address.clone())).await?;
    let workers = state.db.workers_of(&address, now).await?;
    let payouts = state.db.recent_payouts_of(&address, MINER_PAYOUTS).await?;

    if books.is_none() && workers.is_empty() && payouts.is_empty() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "The pool has not seen any work or earnings from this address yet. If you have only just started mining, wait a minute and try again.",
        ));
    }
    let (balance, earned, paid) =
        books.map(|b| (b.balance, b.total_earned, b.total_paid)).unwrap_or((Amount::ZERO, Amount::ZERO, Amount::ZERO));

    let account = state.db.miner_account(&address).await?;
    let chosen = account.as_ref().and_then(|a| a.payout_level);
    let level = chosen.map(|l| l.max(state.min_payout)).unwrap_or(state.min_payout);
    let next_payout_ms = state.payout_schedule.next_run_ms(now);
    let schedule = state.payout_schedule.describe();
    let payout_note = if balance.is_zero() {
        if paid.is_zero() {
            "You have not earned anything yet. Rewards are shared out when the pool finds a block.".to_owned()
        } else {
            "Everything you have earned has been paid.".to_owned()
        }
    } else if balance >= level {
        format!(
            "You are owed {}, which is enough. You will be paid at the next payment run, {schedule}, in about {}.",
            balance.display::<C>(),
            in_words(next_payout_ms.saturating_sub(now))
        )
    } else {
        format!(
            "You are owed {} so far. You are paid once you are owed at least {}, your payout level, at the first payment run after that. Payments are sent {schedule}.",
            balance.display::<C>(),
            level.display::<C>()
        )
    };

    Ok(Json(MinerJson {
        address: address.to_string(),
        balance: money::<C>(balance),
        total_earned: money::<C>(earned),
        total_paid: money::<C>(paid),
        workers: workers
            .into_iter()
            .map(|w| WorkerJson {
                name: w.worker,
                hashrate_hps: hashrate(w.hash_value_online_window),
                last_share_ms: w.last_share_ms,
                seconds_since_last_share: now.saturating_sub(w.last_share_ms) / 1000,
            })
            .collect(),
        payouts: payouts
            .into_iter()
            .map(|p| MinerPayoutJson {
                amount: money::<C>(p.amount),
                status: p.status.as_str(),
                status_text: payout_words(p.status),
                txid: p.txid,
                time_ms: p.updated_ts_ms,
            })
            .collect(),
        password_set: account.is_some(),
        payout_level: money::<C>(level),
        payout_level_chosen: chosen.is_some(),
        lowest_payout_level: money::<C>(state.min_payout),
        highest_payout_level: money::<C>(Amount::from_coins(pool_payout::HIGHEST_PAYOUT_LEVEL_COINS).unwrap_or(Amount::new(u64::MAX))),
        payout_note,
        next_payout_ms,
    }))
}

// ---- lists --------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct LimitQuery {
    limit: Option<String>,
}

fn limit_from(query: LimitQuery) -> Result<u32, ApiError> {
    match query.limit {
        None => Ok(DEFAULT_LIMIT),
        Some(text) => match text.trim().parse::<u32>() {
            Ok(n) if n >= 1 => Ok(n.min(MAX_LIMIT)),
            _ => Err(ApiError::new(StatusCode::BAD_REQUEST, format!("The limit must be a whole number from 1 to {MAX_LIMIT}."))),
        },
    }
}

#[derive(Serialize)]
pub(crate) struct BlockJson {
    hash: String,
    address: String,
    worker: String,
    blue_score: u64,
    daa_score: u64,
    time_ms: u64,
    status: &'static str,
}

impl From<BlockRow> for BlockJson {
    fn from(block: BlockRow) -> Self {
        BlockJson {
            hash: block.hash,
            address: block.address.to_string(),
            worker: block.worker.to_string(),
            blue_score: block.blue_score,
            daa_score: block.daa_score,
            time_ms: block.ts_ms,
            status: block_words(block.blue),
        }
    }
}

pub(crate) async fn blocks(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<Vec<BlockJson>>, ApiError> {
    let limit = limit_from(query)?;
    Ok(Json(state.db.recent_blocks(limit).await?.into_iter().map(BlockJson::from).collect()))
}

#[derive(Serialize)]
pub(crate) struct PayoutJson {
    address: String,
    amount: Money,
    status: &'static str,
    status_text: &'static str,
    txid: Option<String>,
    time_ms: u64,
}

fn payout_json<C: Chain>(p: Payout) -> PayoutJson {
    PayoutJson {
        address: p.address.to_string(),
        amount: money::<C>(p.amount),
        status: p.status.as_str(),
        status_text: payout_words(p.status),
        txid: p.txid,
        time_ms: p.updated_ts_ms,
    }
}

pub(crate) async fn payouts<C: Chain>(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<Vec<PayoutJson>>, ApiError> {
    let limit = limit_from(query)?;
    Ok(Json(state.db.recent_payouts(limit).await?.into_iter().map(payout_json::<C>).collect()))
}
