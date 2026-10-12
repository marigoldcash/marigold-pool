use crate::{Db, DbError, to_i64, to_u64};
use pool_domain::{Amount, MinerAddress};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

/// Where a payout is in its life: written down, handed to the network,
/// confirmed there, or given up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayoutStatus {
    Pending,
    Sent,
    Confirmed,
    Failed,
}

impl PayoutStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PayoutStatus::Pending => "pending",
            PayoutStatus::Sent => "sent",
            PayoutStatus::Confirmed => "confirmed",
            PayoutStatus::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Result<Self, DbError> {
        match text {
            "pending" => Ok(PayoutStatus::Pending),
            "sent" => Ok(PayoutStatus::Sent),
            "confirmed" => Ok(PayoutStatus::Confirmed),
            "failed" => Ok(PayoutStatus::Failed),
            other => Err(DbError::Corrupt(format!("payout status {other:?}"))),
        }
    }
}

/// A payout about to be written down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPayout {
    /// Unique across all payouts, so writing the same payout twice is refused.
    pub idempotency_key: String,
    pub address: MinerAddress,
    pub amount: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payout {
    pub id: i64,
    pub idempotency_key: String,
    pub address: MinerAddress,
    pub amount: Amount,
    pub status: PayoutStatus,
    pub txid: Option<String>,
    /// The change address of the transaction that carries the payments, which
    /// is where to look for it after a restart.
    pub watch_address: Option<String>,
    /// Why a payout failed, for the operator.
    pub note: Option<String>,
    pub created_ts_ms: u64,
    pub updated_ts_ms: u64,
}

fn payout_from_row(row: &SqliteRow) -> Result<Payout, DbError> {
    Ok(Payout {
        id: row.get("id"),
        idempotency_key: row.get("idempotency_key"),
        address: MinerAddress::from_trusted(row.get("address")),
        amount: Amount::new(to_u64(row.get("amount"), "payout amount")?),
        status: PayoutStatus::parse(&row.get::<String, _>("status"))?,
        txid: row.get("txid"),
        watch_address: row.get("watch_address"),
        note: row.get("note"),
        created_ts_ms: to_u64(row.get("created_ts_ms"), "payout created time")?,
        updated_ts_ms: to_u64(row.get("updated_ts_ms"), "payout updated time")?,
    })
}

impl Db {
    /// Write payouts down as `pending` and take their amounts out of the
    /// miners' balances, all in one transaction. If any one of them cannot be
    /// written (a balance too small, a key already used) none are.
    pub async fn create_pending_payouts(&self, items: &[NewPayout], now_ms: u64) -> Result<Vec<Payout>, DbError> {
        let now = to_i64(now_ms, "payout time")?;
        let mut tx = self.wr().begin().await?;
        let mut ids = Vec::with_capacity(items.len());
        for item in items {
            if item.amount.is_zero() {
                return Err(DbError::OutOfRange("a payout of nothing"));
            }
            let taken = sqlx::query("SELECT 1 FROM payouts WHERE idempotency_key = ?1")
                .bind(&item.idempotency_key)
                .fetch_optional(&mut *tx)
                .await?;
            if taken.is_some() {
                return Err(DbError::DuplicatePayoutKey(item.idempotency_key.clone()));
            }
            let have = match sqlx::query("SELECT balance FROM balances WHERE address = ?1")
                .bind(item.address.as_str())
                .fetch_optional(&mut *tx)
                .await?
            {
                Some(row) => to_u64(row.get("balance"), "balance")?,
                None => 0,
            };
            if have < item.amount.units() {
                return Err(DbError::InsufficientBalance { address: item.address.to_string(), have, wanted: item.amount.units() });
            }
            let units = to_i64(item.amount.units(), "payout amount")?;
            sqlx::query("UPDATE balances SET balance = balance - ?1 WHERE address = ?2")
                .bind(units)
                .bind(item.address.as_str())
                .execute(&mut *tx)
                .await?;
            let inserted = sqlx::query(
                "INSERT INTO payouts (idempotency_key, address, amount, status, created_ts_ms, updated_ts_ms) VALUES (?1, ?2, ?3, 'pending', ?4, ?5)",
            )
            .bind(&item.idempotency_key)
            .bind(item.address.as_str())
            .bind(units)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            ids.push(inserted.last_insert_rowid());
        }
        let mut written = Vec::with_capacity(ids.len());
        for id in ids {
            let row = sqlx::query("SELECT * FROM payouts WHERE id = ?1").bind(id).fetch_one(&mut *tx).await?;
            written.push(payout_from_row(&row)?);
        }
        tx.commit().await?;
        Ok(written)
    }

    /// `pending` to `sent`, for a whole batch at once: the transaction that
    /// carries these payments has this id and this change address, and is about
    /// to be (or has been) broadcast. All or none.
    pub async fn mark_payouts_sent(&self, ids: &[i64], txid: &str, watch_address: Option<&str>, now_ms: u64) -> Result<(), DbError> {
        let now = to_i64(now_ms, "payout time")?;
        let mut tx = self.wr().begin().await?;
        for &id in ids {
            let result = sqlx::query("UPDATE payouts SET status = 'sent', txid = ?1, watch_address = ?2, updated_ts_ms = ?3 WHERE id = ?4 AND status = 'pending'")
                .bind(txid)
                .bind(watch_address)
                .bind(now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            if result.rows_affected() != 1 {
                drop(tx);
                return Err(self.wrong_state(id, "sent").await);
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// `pending` to `sent` for one payout.
    pub async fn mark_payout_sent(&self, id: i64, txid: &str, now_ms: u64) -> Result<(), DbError> {
        self.mark_payouts_sent(&[id], txid, None, now_ms).await
    }

    /// `sent` to `confirmed`, for a whole batch: the network has it. Each
    /// amount is added to what that miner has been paid. All or none.
    pub async fn confirm_payouts(&self, ids: &[i64], now_ms: u64) -> Result<(), DbError> {
        let now = to_i64(now_ms, "payout time")?;
        let mut tx = self.wr().begin().await?;
        for &id in ids {
            let row = sqlx::query("SELECT address, amount FROM payouts WHERE id = ?1 AND status = 'sent'")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
            let Some(row) = row else {
                drop(tx);
                return Err(self.wrong_state(id, "confirmed").await);
            };
            let address: String = row.get("address");
            let amount: i64 = row.get("amount");
            sqlx::query("UPDATE payouts SET status = 'confirmed', updated_ts_ms = ?1 WHERE id = ?2")
                .bind(now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE balances SET total_paid = total_paid + ?1 WHERE address = ?2")
                .bind(amount)
                .bind(address)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// `sent` to `confirmed` for one payout.
    pub async fn mark_payout_confirmed(&self, id: i64, now_ms: u64) -> Result<(), DbError> {
        self.confirm_payouts(&[id], now_ms).await
    }

    /// Give up on payouts that are `pending` or `sent`, for a whole batch: each
    /// amount goes back onto that miner's balance and the reason is kept for the
    /// operator. All or none.
    pub async fn fail_payouts(&self, ids: &[i64], note: &str, now_ms: u64) -> Result<(), DbError> {
        let now = to_i64(now_ms, "payout time")?;
        let mut tx = self.wr().begin().await?;
        for &id in ids {
            let row = sqlx::query("SELECT address, amount FROM payouts WHERE id = ?1 AND status IN ('pending', 'sent')")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
            let Some(row) = row else {
                drop(tx);
                return Err(self.wrong_state(id, "failed").await);
            };
            let address: String = row.get("address");
            let amount: i64 = row.get("amount");
            sqlx::query("UPDATE payouts SET status = 'failed', note = ?1, updated_ts_ms = ?2 WHERE id = ?3")
                .bind(note)
                .bind(now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO balances (address, balance, total_earned, total_paid) VALUES (?1, ?2, 0, 0) \
                 ON CONFLICT(address) DO UPDATE SET balance = balance + excluded.balance",
            )
            .bind(address)
            .bind(amount)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Give up on one payout that is `pending` or `sent`.
    pub async fn fail_payout(&self, id: i64, note: &str, now_ms: u64) -> Result<(), DbError> {
        self.fail_payouts(&[id], note, now_ms).await
    }

    /// How many payouts, in any state, have ever been written for this miner.
    pub async fn payout_count(&self, address: &MinerAddress) -> Result<u64, DbError> {
        let row =
            sqlx::query("SELECT COUNT(*) AS n FROM payouts WHERE address = ?1").bind(address.as_str()).fetch_one(self.wr()).await?;
        to_u64(row.get("n"), "payout count")
    }

    pub async fn get_payout(&self, id: i64) -> Result<Option<Payout>, DbError> {
        let row = sqlx::query("SELECT * FROM payouts WHERE id = ?1").bind(id).fetch_optional(self.wr()).await?;
        row.as_ref().map(payout_from_row).transpose()
    }

    pub async fn payout_by_key(&self, idempotency_key: &str) -> Result<Option<Payout>, DbError> {
        let row =
            sqlx::query("SELECT * FROM payouts WHERE idempotency_key = ?1").bind(idempotency_key).fetch_optional(self.wr()).await?;
        row.as_ref().map(payout_from_row).transpose()
    }

    /// Every payout in a given state, oldest first.
    pub async fn payouts_with_status(&self, status: PayoutStatus) -> Result<Vec<Payout>, DbError> {
        let rows =
            sqlx::query("SELECT * FROM payouts WHERE status = ?1 ORDER BY id").bind(status.as_str()).fetch_all(self.wr()).await?;
        rows.iter().map(payout_from_row).collect()
    }

    /// Why a state change found nothing to change: the payout is not there, or
    /// it is somewhere its life cannot continue from.
    async fn wrong_state(&self, id: i64, wanted: &'static str) -> DbError {
        match sqlx::query("SELECT status FROM payouts WHERE id = ?1").bind(id).fetch_optional(self.wr()).await {
            Ok(Some(row)) => DbError::WrongPayoutState { id, found: row.get("status"), wanted },
            Ok(None) => DbError::UnknownPayout(id),
            Err(error) => DbError::Sqlx(error),
        }
    }
}
