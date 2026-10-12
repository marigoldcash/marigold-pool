use crate::{Db, DbError, to_i64, to_u64};
use pool_domain::{MinerAddress, ShareWeight, WorkerName};
use sqlx::Row;

/// A share ready to be stored.
#[derive(Debug, Clone, PartialEq)]
pub struct NewShare {
    pub address: MinerAddress,
    pub worker: WorkerName,
    pub weight: ShareWeight,
    pub hash_value: f64,
    pub daa_score: u64,
    pub ts_ms: u64,
}

impl Db {
    /// Store a batch of shares in one transaction: all of them or none.
    pub async fn insert_shares(&self, shares: &[NewShare]) -> Result<(), DbError> {
        if shares.is_empty() {
            return Ok(());
        }
        let mut tx = self.wr().begin().await?;
        for share in shares {
            sqlx::query("INSERT INTO shares (address, worker, weight, hash_value, daa_score, ts_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
                .bind(share.address.as_str())
                .bind(share.worker.as_str())
                .bind(to_i64(share.weight.units(), "share weight")?)
                .bind(share.hash_value)
                .bind(to_i64(share.daa_score, "share DAA score")?)
                .bind(to_i64(share.ts_ms, "share time")?)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn insert_share(&self, share: &NewShare) -> Result<(), DbError> {
        self.insert_shares(std::slice::from_ref(share)).await
    }

    /// Each miner's summed share weight over the window `(after_daa, up_to_daa]`:
    /// the first bound is left out, the second is included. One row per miner,
    /// in address order, so the same shares always give the same list.
    pub async fn weights_in_window(&self, after_daa: u64, up_to_daa: u64) -> Result<Vec<(MinerAddress, ShareWeight)>, DbError> {
        let rows = sqlx::query(
            "SELECT address, SUM(weight) AS total FROM shares WHERE daa_score > ?1 AND daa_score <= ?2 GROUP BY address ORDER BY address",
        )
        .bind(to_i64(after_daa, "window start")?)
        .bind(to_i64(up_to_daa, "window end")?)
        .fetch_all(self.rd())
        .await?;
        rows.iter()
            .map(|row| {
                let address: String = row.get("address");
                let total: i64 = row.get("total");
                Ok((MinerAddress::from_trusted(address), ShareWeight::new(to_u64(total, "summed weight")?)))
            })
            .collect()
    }

    /// Delete every share older than `daa_score` (strictly below it). Returns
    /// how many were deleted.
    pub async fn prune_shares_below(&self, daa_score: u64) -> Result<u64, DbError> {
        let result =
            sqlx::query("DELETE FROM shares WHERE daa_score < ?1").bind(to_i64(daa_score, "prune point")?).execute(self.wr()).await?;
        Ok(result.rows_affected())
    }

    pub async fn count_shares(&self) -> Result<u64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM shares").fetch_one(self.rd()).await?;
        to_u64(row.get("n"), "share count")
    }

    /// The highest DAA score among stored shares, if there are any.
    pub async fn latest_share_daa(&self) -> Result<Option<u64>, DbError> {
        let row = sqlx::query("SELECT MAX(daa_score) AS top FROM shares").fetch_one(self.rd()).await?;
        let top: Option<i64> = row.get("top");
        top.map(|v| to_u64(v, "latest DAA score")).transpose()
    }
}
