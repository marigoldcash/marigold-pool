use crate::{Db, DbError, bits_to_i64, bits_to_u64, to_i64, to_u64};
use pool_domain::{MinerAddress, WorkerName};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

/// A block the node accepted from one of the pool's miners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBlock {
    pub hash: String,
    pub address: MinerAddress,
    pub worker: WorkerName,
    pub blue_score: u64,
    pub daa_score: u64,
    pub nonce: u64,
    pub ts_ms: u64,
}

/// A stored block. `blue` is `None` until the block's colour is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRow {
    pub hash: String,
    pub address: MinerAddress,
    pub worker: WorkerName,
    pub blue_score: u64,
    pub daa_score: u64,
    pub nonce: u64,
    pub ts_ms: u64,
    pub blue: Option<bool>,
}

fn block_from_row(row: &SqliteRow) -> Result<BlockRow, DbError> {
    let blue: Option<i64> = row.get("blue");
    Ok(BlockRow {
        hash: row.get("hash"),
        address: MinerAddress::from_trusted(row.get("address")),
        worker: WorkerName::new(row.get::<String, _>("worker").as_str()),
        blue_score: to_u64(row.get("blue_score"), "blue score")?,
        daa_score: to_u64(row.get("daa_score"), "block DAA score")?,
        nonce: bits_to_u64(row.get("nonce")),
        ts_ms: to_u64(row.get("ts_ms"), "block time")?,
        blue: blue.map(|v| v != 0),
    })
}

impl Db {
    /// Store a block. Returns `false` and changes nothing if that hash is
    /// already stored, so a block reported twice is counted once.
    pub async fn insert_block(&self, block: &NewBlock) -> Result<bool, DbError> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO blocks (hash, address, worker, blue_score, daa_score, nonce, ts_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&block.hash)
        .bind(block.address.as_str())
        .bind(block.worker.as_str())
        .bind(to_i64(block.blue_score, "blue score")?)
        .bind(to_i64(block.daa_score, "block DAA score")?)
        .bind(bits_to_i64(block.nonce))
        .bind(to_i64(block.ts_ms, "block time")?)
        .execute(self.wr())
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn get_block(&self, hash: &str) -> Result<Option<BlockRow>, DbError> {
        let row = sqlx::query("SELECT * FROM blocks WHERE hash = ?1").bind(hash).fetch_optional(self.rd()).await?;
        row.as_ref().map(block_from_row).transpose()
    }

    /// Blocks whose colour is not yet known and that were found at or after
    /// `min_ts_ms`, oldest first.
    pub async fn unresolved_blocks_since(&self, min_ts_ms: u64) -> Result<Vec<BlockRow>, DbError> {
        let rows = sqlx::query("SELECT * FROM blocks WHERE blue IS NULL AND ts_ms >= ?1 ORDER BY ts_ms, hash")
            .bind(to_i64(min_ts_ms, "block time")?)
            .fetch_all(self.wr())
            .await?;
        rows.iter().map(block_from_row).collect()
    }

    /// Record a block's colour. Returns `false` if the block is not stored.
    pub async fn set_block_blue(&self, hash: &str, blue: bool) -> Result<bool, DbError> {
        let result =
            sqlx::query("UPDATE blocks SET blue = ?1 WHERE hash = ?2").bind(i64::from(blue)).bind(hash).execute(self.wr()).await?;
        Ok(result.rows_affected() == 1)
    }
}
