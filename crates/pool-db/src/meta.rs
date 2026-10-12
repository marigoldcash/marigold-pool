use crate::{Db, DbError};
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
