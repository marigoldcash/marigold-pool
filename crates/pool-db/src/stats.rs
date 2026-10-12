//! The read-only questions the API asks. They all use the read pool.

use crate::payouts::{Payout, PayoutStatus, payout_from_row};
use crate::{BlockRow, Db, DbError, to_i64, to_u64};
use pool_domain::{Amount, MinerAddress, POOL_ACCOUNT};
use sqlx::Row;

/// How far back a miner or worker still counts as online: ten minutes.
pub const ONLINE_WINDOW_MS: u64 = 10 * 60 * 1000;

/// A day, for "blocks found today".
pub const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// The pool as a whole, as of one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolStats {
    pub miners_online: u64,
    pub workers_online: u64,
    pub shares_online_window: u64,
    /// The expected number of hashes all shares in the online window stand for.
    /// Divide by the window's length in seconds for the pool's hashrate.
    pub hash_value_online_window: f64,
    pub blocks_24h: u64,
    pub last_block_ms: Option<u64>,
    pub last_share_ms: Option<u64>,
    /// What miners are owed right now, the pool's own fee not counted.
    pub owed_to_miners: Amount,
}

/// One of a miner's workers.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerStats {
    pub worker: String,
    pub last_share_ms: u64,
    pub shares_online_window: u64,
    pub hash_value_online_window: f64,
}

/// How many payouts are in each state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PayoutCounts {
    pub pending: u64,
    pub sent: u64,
    pub confirmed: u64,
    pub failed: u64,
}

fn count(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<u64, DbError> {
    to_u64(row.get(column), "a count")
}

impl Db {
    pub async fn pool_stats(&self, now_ms: u64) -> Result<PoolStats, DbError> {
        let since = to_i64(now_ms.saturating_sub(ONLINE_WINDOW_MS), "online window")?;
        let day_since = to_i64(now_ms.saturating_sub(DAY_MS), "day window")?;

        let shares = sqlx::query(
            "SELECT COUNT(DISTINCT address) AS miners, COUNT(DISTINCT address || char(0) || worker) AS workers, \
             COUNT(*) AS shares, COALESCE(SUM(hash_value), 0.0) AS hashes, MAX(ts_ms) AS last_share \
             FROM shares WHERE ts_ms >= ?1",
        )
        .bind(since)
        .fetch_one(self.rd())
        .await?;
        // The newest share, whether or not it is inside the window.
        let newest = sqlx::query("SELECT MAX(ts_ms) AS last_share FROM shares").fetch_one(self.rd()).await?;
        let blocks = sqlx::query("SELECT COUNT(*) AS n, MAX(ts_ms) AS last FROM blocks WHERE ts_ms >= ?1")
            .bind(day_since)
            .fetch_one(self.rd())
            .await?;
        let last_block = sqlx::query("SELECT MAX(ts_ms) AS last FROM blocks").fetch_one(self.rd()).await?;
        let owed = sqlx::query("SELECT COALESCE(SUM(balance), 0) AS owed FROM balances WHERE address != ?1")
            .bind(POOL_ACCOUNT)
            .fetch_one(self.rd())
            .await?;

        let opt = |v: Option<i64>, what: &'static str| v.map(|v| to_u64(v, what)).transpose();
        Ok(PoolStats {
            miners_online: count(&shares, "miners")?,
            workers_online: count(&shares, "workers")?,
            shares_online_window: count(&shares, "shares")?,
            hash_value_online_window: shares.get("hashes"),
            blocks_24h: count(&blocks, "n")?,
            last_block_ms: opt(last_block.get("last"), "last block time")?,
            last_share_ms: opt(newest.get("last_share"), "last share time")?,
            owed_to_miners: Amount::new(to_u64(owed.get("owed"), "owed")?),
        })
    }

    /// A miner's workers that have submitted shares recently enough to still be
    /// stored, most recently active first.
    pub async fn workers_of(&self, address: &MinerAddress, now_ms: u64) -> Result<Vec<WorkerStats>, DbError> {
        let since = to_i64(now_ms.saturating_sub(ONLINE_WINDOW_MS), "online window")?;
        let rows = sqlx::query(
            "SELECT worker, MAX(ts_ms) AS last_share, \
             SUM(CASE WHEN ts_ms >= ?2 THEN 1 ELSE 0 END) AS recent_shares, \
             SUM(CASE WHEN ts_ms >= ?2 THEN hash_value ELSE 0.0 END) AS recent_hashes \
             FROM shares WHERE address = ?1 GROUP BY worker ORDER BY last_share DESC, worker",
        )
        .bind(address.as_str())
        .bind(since)
        .fetch_all(self.rd())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(WorkerStats {
                    worker: row.get("worker"),
                    last_share_ms: to_u64(row.get("last_share"), "worker last share")?,
                    shares_online_window: count(row, "recent_shares")?,
                    hash_value_online_window: row.get("recent_hashes"),
                })
            })
            .collect()
    }

    /// The newest blocks the pool found, newest first.
    pub async fn recent_blocks(&self, limit: u32) -> Result<Vec<BlockRow>, DbError> {
        let rows =
            sqlx::query("SELECT * FROM blocks ORDER BY ts_ms DESC, hash LIMIT ?1").bind(i64::from(limit)).fetch_all(self.rd()).await?;
        rows.iter().map(crate::blocks::block_from_row).collect()
    }

    /// The newest payouts, newest first.
    pub async fn recent_payouts(&self, limit: u32) -> Result<Vec<Payout>, DbError> {
        let rows = sqlx::query("SELECT * FROM payouts ORDER BY id DESC LIMIT ?1").bind(i64::from(limit)).fetch_all(self.rd()).await?;
        rows.iter().map(payout_from_row).collect()
    }

    /// One miner's newest payouts, newest first.
    pub async fn recent_payouts_of(&self, address: &MinerAddress, limit: u32) -> Result<Vec<Payout>, DbError> {
        let rows = sqlx::query("SELECT * FROM payouts WHERE address = ?1 ORDER BY id DESC LIMIT ?2")
            .bind(address.as_str())
            .bind(i64::from(limit))
            .fetch_all(self.rd())
            .await?;
        rows.iter().map(payout_from_row).collect()
    }

    pub async fn payout_counts(&self) -> Result<PayoutCounts, DbError> {
        let rows = sqlx::query("SELECT status, COUNT(*) AS n FROM payouts GROUP BY status").fetch_all(self.rd()).await?;
        let mut counts = PayoutCounts::default();
        for row in &rows {
            let n = count(row, "n")?;
            match row.get::<String, _>("status").as_str() {
                s if s == PayoutStatus::Pending.as_str() => counts.pending = n,
                s if s == PayoutStatus::Sent.as_str() => counts.sent = n,
                s if s == PayoutStatus::Confirmed.as_str() => counts.confirmed = n,
                s if s == PayoutStatus::Failed.as_str() => counts.failed = n,
                other => return Err(DbError::Corrupt(format!("payout status {other:?}"))),
            }
        }
        Ok(counts)
    }

    /// Whether the database answers at all.
    pub async fn ping(&self) -> Result<(), DbError> {
        sqlx::query("SELECT 1").fetch_one(self.rd()).await?;
        Ok(())
    }
}
