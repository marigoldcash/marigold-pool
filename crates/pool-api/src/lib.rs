//! The pool's read-only face: a JSON API, one static page, and the metrics a
//! monitor scrapes. Nothing here can change the pool's books.
//!
//! Bind it to the loopback address and put a web server in front of it for the
//! public (that part is the operator's), or leave it local.

mod handlers;
mod metrics;
mod passwords;
mod settings;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::routing::{get, post};
use pool_accountant::AccountantMetrics;
use pool_chain::{Chain, SharedNode};
use pool_db::Db;
use pool_domain::Amount;
use pool_payout::Schedule;
use std::sync::Arc;
use tokio::sync::{Semaphore, watch};

/// The current time in milliseconds since the Unix epoch. A function, so tests
/// can stop the clock.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The real clock.
pub fn system_clock() -> Clock {
    Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

/// Everything the API needs to answer.
pub struct ApiState {
    pub db: Db,
    /// The network id, such as "testnet-10".
    pub network: String,
    /// The address block rewards are paid to.
    pub pool_address: String,
    pub fee_bp: u16,
    pub min_payout: Amount,
    pub window_daa: u64,
    /// The accountant's counters, for the metrics page.
    pub metrics: Arc<AccountantMetrics>,
    /// The node, for the health check. `None`: the node is not checked.
    pub node: Option<SharedNode>,
    pub clock: Clock,
    /// When payments go out, for the pages to say so.
    pub payout_schedule: Schedule,
    /// How many passwords may be checked at the same moment; see `password_gate`.
    pub password_work: Arc<Semaphore>,
}

/// Passwords are hashed slowly on purpose. At most this many are checked at once;
/// anyone else is asked to try again, so a flood of attempts cannot starve the pool.
pub fn password_gate() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(2))
}

/// The biggest request body accepted: a password and a number need far less.
const MAX_BODY_BYTES: usize = 4096;

/// All the pages and endpoints.
pub fn router<C: Chain + 'static>(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/", get(handlers::dashboard))
        .route("/health", get(handlers::health))
        .route("/api/pool", get(handlers::pool::<C>))
        .route("/api/miner/{address}", get(handlers::miner::<C>))
        .route("/api/miner/{address}/password", post(settings::set_password::<C>).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)))
        .route("/api/miner/{address}/settings", post(settings::change_settings::<C>).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)))
        .route("/api/blocks", get(handlers::blocks))
        .route("/api/payouts", get(handlers::payouts::<C>))
        .route("/metrics", get(metrics::metrics))
        .layer(axum::middleware::map_response(plain_headers))
        .with_state(state)
}

/// Every response: never cached, and never guessed at.
async fn plain_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// Answer requests on `listener` until `shutdown` changes. Requests already
/// being answered are finished first.
pub async fn serve<C: Chain + 'static>(
    state: Arc<ApiState>,
    listener: tokio::net::TcpListener,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    axum::serve(listener, router::<C>(state))
        .with_graceful_shutdown(async move {
            let _ = shutdown.wait_for(|stop| *stop).await;
        })
        .await
}

#[cfg(test)]
mod tests;
