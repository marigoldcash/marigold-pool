use crate::{Db, DbError, to_i64};
use sqlx::Row;

impl Db {
    pub async fn meta_get(&self, key: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query("SELECT value FROM meta WHERE key = ?1").bind(key).fetch_optional(self.wr()).await?;
        Ok(row.map(|r| r.get("value")))
    }

    pub async fn meta_set(&self, key: &str, value: &str) -> Result<(), DbError> {
        sqlx::query("INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(key)
            .bind(value)
            .execute(self.wr())
            .await?;
        Ok(())
    }
}

impl Db {
    /// Take a named lock for `owner` for `ttl_ms`, so that two processes sharing
    /// this database (the running pool and a person typing `payout --now`) cannot
    /// do the same job at once. Returns `false` if somebody else holds it and it
    /// has not run out. A lock that has run out can be taken by anyone, so a
    /// crashed holder cannot block the job for ever. The holder can take it
    /// again to extend it.
    pub async fn try_lock(&self, name: &str, owner: &str, now_ms: u64, ttl_ms: u64) -> Result<bool, DbError> {
        let key = format!("lock:{name}");
        let expires = now_ms.saturating_add(ttl_ms);
        let mut tx = self.wr().begin().await?;
        let held = sqlx::query("SELECT value FROM meta WHERE key = ?1").bind(&key).fetch_optional(&mut *tx).await?;
        if let Some(row) = held {
            let value: String = sqlx::Row::get(&row, "value");
            if let Some((current_owner, until)) = value.rsplit_once('|') {
                let until: u64 = until.parse().unwrap_or(0);
                if current_owner != owner && until > now_ms {
                    return Ok(false);
                }
            }
        }
        sqlx::query("INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(&key)
            .bind(format!("{owner}|{}", to_i64(expires, "lock time")?))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Give a lock back. Does nothing if somebody else holds it now.
    pub async fn unlock(&self, name: &str, owner: &str) -> Result<(), DbError> {
        let key = format!("lock:{name}");
        sqlx::query("DELETE FROM meta WHERE key = ?1 AND value LIKE ?2")
            .bind(key)
            .bind(format!("{owner}|%"))
            .execute(self.wr())
            .await?;
        Ok(())
    }
}
