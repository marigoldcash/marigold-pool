//! Storage for the pool: one SQLite file for the whole pool, never one per
//! miner, because splitting a reward reads every miner's shares at once.
//!
//! The locking rule, so that "database is locked" never appears: the file is in
//! WAL mode, and there is exactly **one writer connection**. Every task that
//! writes (the share consumer, the reward watcher, the allocation runner, the
//! payout cycle) shares it, so their writes queue inside this process instead
//! of fighting for SQLite's single write lock. The API reads through a separate
//! read-only pool, and in WAL mode readers and the writer never block each
//! other.

mod accounts;
mod balances;
mod blocks;
mod error;
mod meta;
mod miners;
mod payouts;
mod rewards;
mod shares;
mod stats;
#[cfg(test)]
mod tests;

pub use accounts::MinerAccount;
pub use balances::{Balance, EligibleBalance};
pub use blocks::{BlockRow, NewBlock};
pub use error::DbError;
pub use payouts::{NewPayout, Payout, PayoutStatus};
pub use rewards::Reward;
pub use shares::NewShare;
pub use stats::{DAY_MS, ONLINE_WINDOW_MS, PayoutCounts, PoolStats, WorkerStats};

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// How long a connection waits for SQLite's lock before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How many read-only connections the API gets.
const READ_CONNECTIONS: u32 = 4;

/// The pool's database. Cheap to clone; clones share the same connections.
#[derive(Clone)]
pub struct Db {
    write: SqlitePool,
    read: SqlitePool,
}

impl Db {
    /// Open (creating it if it is missing) the database file at `path` and bring
    /// its schema up to date. Opening a file that is already up to date changes
    /// nothing.
    pub async fn open(path: &Path) -> Result<Db, DbError> {
        let write_options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT);
        let write = SqlitePoolOptions::new().max_connections(1).connect_with(write_options).await?;
        MIGRATOR.run(&write).await?;

        // The readers open after the writer has created the file and set WAL.
        let read_options = SqliteConnectOptions::new().filename(path).read_only(true).busy_timeout(BUSY_TIMEOUT);
        let read = SqlitePoolOptions::new().max_connections(READ_CONNECTIONS).connect_with(read_options).await?;
        Ok(Db { write, read })
    }

    /// A database that lives in memory, for tests. An in-memory database exists
    /// only inside the one connection that made it, so reads and writes share
    /// that connection here.
    pub async fn open_in_memory() -> Result<Db, DbError> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Db { write: pool.clone(), read: pool })
    }

    /// The connection every write goes through.
    pub(crate) fn wr(&self) -> &SqlitePool {
        &self.write
    }

    /// The read-only pool, for queries that do not feed a write.
    pub(crate) fn rd(&self) -> &SqlitePool {
        &self.read
    }

    /// The read-only pool, for the API's own queries.
    pub fn read_pool(&self) -> &SqlitePool {
        &self.read
    }

    /// Close every connection. Waits for statements already running.
    pub async fn close(&self) {
        self.write.close().await;
        self.read.close().await;
    }
}

/// A `u64` as the `i64` SQLite stores, refusing what does not fit.
pub(crate) fn to_i64(value: u64, what: &'static str) -> Result<i64, DbError> {
    i64::try_from(value).map_err(|_| DbError::OutOfRange(what))
}

/// A stored `i64` back as a `u64`, refusing a negative one.
pub(crate) fn to_u64(value: i64, what: &'static str) -> Result<u64, DbError> {
    u64::try_from(value).map_err(|_| DbError::OutOfRange(what))
}

/// A `u64` with all its bits kept, as the `i64` that has the same bits.
pub(crate) fn bits_to_i64(value: u64) -> i64 {
    value as i64
}

/// The inverse of `bits_to_i64`.
pub(crate) fn bits_to_u64(value: i64) -> u64 {
    value as u64
}
