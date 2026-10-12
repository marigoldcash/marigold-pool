//! What a monitor scrapes. Counters come from the accountant's own counters;
//! the rest are read from the database at the moment of the scrape.

use crate::ApiState;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use pool_db::ONLINE_WINDOW_MS;
use prometheus::{Encoder, Gauge, IntCounter, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::error;

fn counter(registry: &Registry, name: &str, help: &str, value: u64) -> prometheus::Result<()> {
    let counter = IntCounter::new(name, help)?;
    counter.inc_by(value);
    registry.register(Box::new(counter))
}

fn gauge(registry: &Registry, name: &str, help: &str, value: i64) -> prometheus::Result<()> {
    let gauge = IntGauge::new(name, help)?;
    gauge.set(value);
    registry.register(Box::new(gauge))
}

fn build(state: &ApiState, stats: &pool_db::PoolStats, payouts: pool_db::PayoutCounts) -> prometheus::Result<Registry> {
    let registry = Registry::new();
    let m = &state.metrics;
    let read = |c: &std::sync::atomic::AtomicU64| c.load(Ordering::Relaxed);

    counter(&registry, "marigold_pool_shares_stored_total", "Shares counted since the pool started.", read(&m.shares_stored))?;
    counter(
        &registry,
        "marigold_pool_shares_rejected_total",
        "Shares not counted (wrong network, no difficulty, numbers too large).",
        read(&m.shares_rejected),
    )?;
    counter(&registry, "marigold_pool_shares_pruned_total", "Old shares deleted.", read(&m.shares_pruned))?;
    counter(&registry, "marigold_pool_blocks_found_total", "Blocks recorded since the pool started.", read(&m.blocks_stored))?;
    counter(
        &registry,
        "marigold_pool_events_lagged_total",
        "Bridge events missed because the pool fell behind.",
        read(&m.events_lagged),
    )?;
    counter(
        &registry,
        "marigold_pool_rewards_seen_total",
        "Block rewards seen arriving at the pool's address.",
        read(&m.rewards_seen),
    )?;
    counter(&registry, "marigold_pool_rewards_allocated_total", "Block rewards split among the miners.", read(&m.rewards_allocated))?;
    counter(&registry, "marigold_pool_node_errors_total", "Looks at the node that failed.", read(&m.node_errors))?;
    counter(&registry, "marigold_pool_write_errors_total", "Writes to the database that failed.", read(&m.flush_errors))?;

    gauge(&registry, "marigold_pool_miners_online", "Miners with work in the last ten minutes.", stats.miners_online as i64)?;
    gauge(&registry, "marigold_pool_workers_online", "Machines with work in the last ten minutes.", stats.workers_online as i64)?;
    gauge(&registry, "marigold_pool_blocks_24h", "Blocks found in the last day.", stats.blocks_24h as i64)?;
    gauge(
        &registry,
        "marigold_pool_owed_units",
        "What miners are owed now, in the chain's smallest unit.",
        stats.owed_to_miners.units() as i64,
    )?;

    let speed = Gauge::new("marigold_pool_hashrate_hps", "The pool's speed over the last ten minutes, in hashes per second.")?;
    speed.set(stats.hash_value_online_window / (ONLINE_WINDOW_MS as f64 / 1000.0));
    registry.register(Box::new(speed))?;

    let by_state = IntGaugeVec::new(Opts::new("marigold_pool_payouts", "Payouts by state."), &["status"])?;
    by_state.with_label_values(&["pending"]).set(payouts.pending as i64);
    by_state.with_label_values(&["sent"]).set(payouts.sent as i64);
    by_state.with_label_values(&["confirmed"]).set(payouts.confirmed as i64);
    by_state.with_label_values(&["failed"]).set(payouts.failed as i64);
    registry.register(Box::new(by_state))?;
    Ok(registry)
}

pub(crate) async fn metrics(State(state): State<Arc<ApiState>>) -> Response {
    let now = (state.clock)();
    let (stats, payouts) = match (state.db.pool_stats(now).await, state.db.payout_counts().await) {
        (Ok(stats), Ok(payouts)) => (stats, payouts),
        (Err(e), _) | (_, Err(e)) => {
            error!("metrics could not read the database: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "the database is unavailable\n").into_response();
        }
    };
    let registry = match build(&state, &stats, payouts) {
        Ok(registry) => registry,
        Err(e) => {
            error!("metrics could not be built: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "metrics could not be built\n").into_response();
        }
    };
    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();
    if let Err(e) = encoder.encode(&registry.gather(), &mut buffer) {
        error!("metrics could not be written: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "metrics could not be written\n").into_response();
    }
    ([(header::CONTENT_TYPE, encoder.format_type().to_owned())], buffer).into_response()
}
