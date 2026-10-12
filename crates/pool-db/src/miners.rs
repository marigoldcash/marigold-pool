use crate::{Db, DbError, to_i64, to_u64};
use pool_domain::MinerAddress;
use sqlx::Row;

impl Db {
    /// Note that a miner was seen at `ts_ms`. The first time sets when they were
    /// first seen; later calls only ever move the last-seen time forward.
    pub async fn upsert_miner(&self, address: &MinerAddress, ts_ms: u64) -> Result<(), DbError> {
        let ts = to_i64(ts_ms, "miner time")?;
        sqlx::query(
            "INSERT INTO miners (address, first_seen, last_seen) VALUES (?1, ?2, ?3) \
             ON CONFLICT(address) DO UPDATE SET last_seen = MAX(last_seen, excluded.last_seen)",
        )
        .bind(address.as_str())
        .bind(ts)
        .bind(ts)
        .execute(self.wr())
        .await?;
        Ok(())
    }

    /// When a miner was first and last seen, in milliseconds.
    pub async fn miner_times(&self, address: &MinerAddress) -> Result<Option<(u64, u64)>, DbError> {
        let row = sqlx::query("SELECT first_seen, last_seen FROM miners WHERE address = ?1")
            .bind(address.as_str())
            .fetch_optional(self.rd())
            .await?;
        row.map(|r| Ok((to_u64(r.get("first_seen"), "first seen")?, to_u64(r.get("last_seen"), "last seen")?))).transpose()
    }

    pub async fn count_miners(&self) -> Result<u64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM miners").fetch_one(self.rd()).await?;
        to_u64(row.get("n"), "miner count")
    }
}
