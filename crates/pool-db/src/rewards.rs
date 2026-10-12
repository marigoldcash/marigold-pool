use crate::{Db, DbError, to_i64, to_u64};
use pool_domain::Amount;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

/// A coinbase output that arrived at the pool's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reward {
    pub outpoint: String,
    pub amount: Amount,
    pub block_daa_score: u64,
    pub seen_ts_ms: u64,
    /// `None` until the reward has been split among the miners.
    pub allocated_ts_ms: Option<u64>,
}

fn reward_from_row(row: &SqliteRow) -> Result<Reward, DbError> {
    let allocated: Option<i64> = row.get("allocated_ts_ms");
    Ok(Reward {
        outpoint: row.get("outpoint"),
        amount: Amount::new(to_u64(row.get("amount"), "reward amount")?),
        block_daa_score: to_u64(row.get("block_daa_score"), "reward DAA score")?,
        seen_ts_ms: to_u64(row.get("seen_ts_ms"), "reward seen time")?,
        allocated_ts_ms: allocated.map(|v| to_u64(v, "reward allocated time")).transpose()?,
    })
}

impl Db {
    /// Remember a reward the first time it is seen. Returns `true` if it was
    /// new and `false` if it was already known, in which case nothing changes.
    pub async fn record_reward_if_new(
        &self,
        outpoint: &str,
        amount: Amount,
        block_daa_score: u64,
        seen_ts_ms: u64,
    ) -> Result<bool, DbError> {
        let result =
            sqlx::query("INSERT OR IGNORE INTO rewards (outpoint, amount, block_daa_score, seen_ts_ms) VALUES (?1, ?2, ?3, ?4)")
                .bind(outpoint)
                .bind(to_i64(amount.units(), "reward amount")?)
                .bind(to_i64(block_daa_score, "reward DAA score")?)
                .bind(to_i64(seen_ts_ms, "reward seen time")?)
                .execute(self.wr())
                .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Every reward not yet split, oldest block first. Whether each is mature
    /// is the caller's question: it needs the node's current DAA score.
    pub async fn unallocated_rewards(&self) -> Result<Vec<Reward>, DbError> {
        let rows = sqlx::query("SELECT * FROM rewards WHERE allocated_ts_ms IS NULL ORDER BY block_daa_score, outpoint")
            .fetch_all(self.wr())
            .await?;
        rows.iter().map(reward_from_row).collect()
    }

    pub async fn get_reward(&self, outpoint: &str) -> Result<Option<Reward>, DbError> {
        let row = sqlx::query("SELECT * FROM rewards WHERE outpoint = ?1").bind(outpoint).fetch_optional(self.rd()).await?;
        row.as_ref().map(reward_from_row).transpose()
    }
}
