use crate::{Db, DbError, to_i64, to_u64};
use pool_domain::{Amount, MinerAddress};
use sqlx::Row;

/// A miner's own settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinerAccount {
    pub address: MinerAddress,
    /// The password, hashed. The password itself is never stored.
    pub password_hash: String,
    /// The balance this miner chose to be paid at, or `None` for the pool's own minimum.
    pub payout_level: Option<Amount>,
    pub failed_attempts: u32,
    /// No password is tried before this time (milliseconds since the epoch); 0 when not locked.
    pub locked_until_ms: u64,
}

impl Db {
    pub async fn miner_account(&self, address: &MinerAddress) -> Result<Option<MinerAccount>, DbError> {
        let row =
            sqlx::query("SELECT * FROM miner_accounts WHERE address = ?1").bind(address.as_str()).fetch_optional(self.wr()).await?;
        row.map(|row| {
            let level: Option<i64> = row.get("payout_level");
            Ok(MinerAccount {
                address: MinerAddress::from_trusted(row.get("address")),
                password_hash: row.get("password_hash"),
                payout_level: level.map(|v| to_u64(v, "payout level").map(Amount::new)).transpose()?,
                failed_attempts: u32::try_from(to_u64(row.get("failed_attempts"), "failed attempts")?).unwrap_or(u32::MAX),
                locked_until_ms: to_u64(row.get("locked_until_ms"), "lock time")?,
            })
        })
        .transpose()
    }

    /// Make a miner's account. Refused if one already exists, so a password can
    /// only be set once; changing it goes through `update_miner_account`.
    pub async fn create_miner_account(
        &self,
        address: &MinerAddress,
        password_hash: &str,
        payout_level: Option<Amount>,
        now_ms: u64,
    ) -> Result<(), DbError> {
        let now = to_i64(now_ms, "account time")?;
        let level = payout_level.map(|l| to_i64(l.units(), "payout level")).transpose()?;
        let result = sqlx::query("INSERT OR IGNORE INTO miner_accounts (address, password_hash, payout_level, created_ts_ms, updated_ts_ms) VALUES (?1, ?2, ?3, ?4, ?5)")
            .bind(address.as_str())
            .bind(password_hash)
            .bind(level)
            .bind(now)
            .bind(now)
            .execute(self.wr())
            .await?;
        if result.rows_affected() == 1 { Ok(()) } else { Err(DbError::AccountExists(address.to_string())) }
    }

    /// Change a miner's settings. Whatever is `None` is left as it is.
    pub async fn update_miner_account(
        &self,
        address: &MinerAddress,
        new_password_hash: Option<&str>,
        new_level: Option<Amount>,
        now_ms: u64,
    ) -> Result<(), DbError> {
        let now = to_i64(now_ms, "account time")?;
        let level = new_level.map(|l| to_i64(l.units(), "payout level")).transpose()?;
        let result = sqlx::query(
            "UPDATE miner_accounts SET password_hash = COALESCE(?2, password_hash), payout_level = COALESCE(?3, payout_level), updated_ts_ms = ?4 WHERE address = ?1",
        )
        .bind(address.as_str())
        .bind(new_password_hash)
        .bind(level)
        .bind(now)
        .execute(self.wr())
        .await?;
        if result.rows_affected() == 1 { Ok(()) } else { Err(DbError::UnknownAccount(address.to_string())) }
    }

    /// Count a wrong password. After `max_failures` in a row the account is
    /// locked for `lock_ms`. Returns the time the lock ends, or 0 if it is not locked.
    pub async fn note_wrong_password(
        &self,
        address: &MinerAddress,
        now_ms: u64,
        max_failures: u32,
        lock_ms: u64,
    ) -> Result<u64, DbError> {
        let mut tx = self.wr().begin().await?;
        let row = sqlx::query("SELECT failed_attempts FROM miner_accounts WHERE address = ?1")
            .bind(address.as_str())
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else { return Ok(0) };
        let failures = to_u64(row.get("failed_attempts"), "failed attempts")?.saturating_add(1);
        let (kept, locked_until) =
            if failures >= u64::from(max_failures) { (0, now_ms.saturating_add(lock_ms)) } else { (failures, 0) };
        sqlx::query("UPDATE miner_accounts SET failed_attempts = ?2, locked_until_ms = ?3 WHERE address = ?1")
            .bind(address.as_str())
            .bind(to_i64(kept, "failed attempts")?)
            .bind(to_i64(locked_until, "lock time")?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(locked_until)
    }

    /// A right password clears the count of wrong ones.
    pub async fn note_right_password(&self, address: &MinerAddress) -> Result<(), DbError> {
        sqlx::query("UPDATE miner_accounts SET failed_attempts = 0, locked_until_ms = 0 WHERE address = ?1")
            .bind(address.as_str())
            .execute(self.wr())
            .await?;
        Ok(())
    }

    /// Remove a miner's settings and password (the operator's way out when a
    /// password is lost or an address was claimed by someone else). They go back
    /// to the pool's defaults and can set a new password. Returns whether there was one.
    pub async fn reset_miner_account(&self, address: &MinerAddress) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM miner_accounts WHERE address = ?1").bind(address.as_str()).execute(self.wr()).await?;
        Ok(result.rows_affected() == 1)
    }

    /// Whether the pool has ever seen this address mine or earn. A password can
    /// only be set for an address it has.
    pub async fn address_is_known(&self, address: &MinerAddress) -> Result<bool, DbError> {
        let row = sqlx::query(
            "SELECT (EXISTS (SELECT 1 FROM miners WHERE address = ?1) OR EXISTS (SELECT 1 FROM balances WHERE address = ?1) OR EXISTS (SELECT 1 FROM payouts WHERE address = ?1)) AS known",
        )
        .bind(address.as_str())
        .fetch_one(self.wr())
        .await?;
        let known: i64 = row.get("known");
        Ok(known != 0)
    }
}
