use crate::{Db, DbError, to_i64, to_u64};
use pool_domain::{Account, Amount, MinerAddress, POOL_ACCOUNT};
use sqlx::Row;
use std::collections::HashSet;

/// What an account is owed and has been paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Balance {
    pub balance: Amount,
    pub total_earned: Amount,
    pub total_paid: Amount,
}

/// A miner owed at least the payout threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EligibleBalance {
    pub address: MinerAddress,
    pub balance: Amount,
}

impl Db {
    /// Split one reward, completely or not at all. In a single transaction this
    /// marks the reward split, records each share of it, and adds each to that
    /// account's balance and total earned.
    ///
    /// It is refused, and nothing changes, if the reward is unknown, if it was
    /// already split, if the entries name an account twice, or if they do not
    /// add up to the reward exactly. Entries of zero are dropped.
    pub async fn apply_allocation(&self, reward_outpoint: &str, entries: &[(Account, Amount)], now_ms: u64) -> Result<(), DbError> {
        let mut seen = HashSet::new();
        for (account, _) in entries {
            if !seen.insert(account.key()) {
                return Err(DbError::DuplicateAccount(account.key().to_owned()));
            }
        }
        let mut got: u64 = 0;
        for (_, amount) in entries {
            got = got.checked_add(amount.units()).ok_or(DbError::OutOfRange("allocation total"))?;
        }

        let mut tx = self.wr().begin().await?;
        let row = sqlx::query("SELECT amount, allocated_ts_ms FROM rewards WHERE outpoint = ?1")
            .bind(reward_outpoint)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| DbError::UnknownReward(reward_outpoint.to_owned()))?;
        let allocated: Option<i64> = row.get("allocated_ts_ms");
        if allocated.is_some() {
            return Err(DbError::AlreadyAllocated(reward_outpoint.to_owned()));
        }
        let expected = to_u64(row.get("amount"), "reward amount")?;
        if got != expected {
            return Err(DbError::AllocationMismatch { outpoint: reward_outpoint.to_owned(), expected, got });
        }

        let marked = sqlx::query("UPDATE rewards SET allocated_ts_ms = ?1 WHERE outpoint = ?2 AND allocated_ts_ms IS NULL")
            .bind(to_i64(now_ms, "allocation time")?)
            .bind(reward_outpoint)
            .execute(&mut *tx)
            .await?;
        if marked.rows_affected() != 1 {
            return Err(DbError::AlreadyAllocated(reward_outpoint.to_owned()));
        }

        for (account, amount) in entries {
            if amount.is_zero() {
                continue;
            }
            let units = to_i64(amount.units(), "allocation amount")?;
            sqlx::query("INSERT INTO allocations (reward_outpoint, address, amount) VALUES (?1, ?2, ?3)")
                .bind(reward_outpoint)
                .bind(account.key())
                .bind(units)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO balances (address, balance, total_earned, total_paid) VALUES (?1, ?2, ?3, 0) \
                 ON CONFLICT(address) DO UPDATE SET balance = balance + excluded.balance, total_earned = total_earned + excluded.total_earned",
            )
            .bind(account.key())
            .bind(units)
            .bind(units)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// How a reward was split, in account order.
    pub async fn allocations_for_reward(&self, reward_outpoint: &str) -> Result<Vec<(Account, Amount)>, DbError> {
        let rows = sqlx::query("SELECT address, amount FROM allocations WHERE reward_outpoint = ?1 ORDER BY address")
            .bind(reward_outpoint)
            .fetch_all(self.rd())
            .await?;
        rows.iter()
            .map(|row| Ok((Account::from_key(row.get("address")), Amount::new(to_u64(row.get("amount"), "allocation amount")?))))
            .collect()
    }

    /// An account's books, or `None` if it has never been credited.
    pub async fn balance_of(&self, account: &Account) -> Result<Option<Balance>, DbError> {
        let row = sqlx::query("SELECT balance, total_earned, total_paid FROM balances WHERE address = ?1")
            .bind(account.key())
            .fetch_optional(self.rd())
            .await?;
        row.map(|r| {
            Ok(Balance {
                balance: Amount::new(to_u64(r.get("balance"), "balance")?),
                total_earned: Amount::new(to_u64(r.get("total_earned"), "total earned")?),
                total_paid: Amount::new(to_u64(r.get("total_paid"), "total paid")?),
            })
        })
        .transpose()
    }

    /// Miners owed at least `minimum` (and always more than nothing), in
    /// address order. The pool's own fee account is never in this list.
    pub async fn balances_eligible(&self, minimum: Amount) -> Result<Vec<EligibleBalance>, DbError> {
        let rows = sqlx::query(
            "SELECT address, balance FROM balances WHERE address != ?1 AND balance > 0 AND balance >= ?2 ORDER BY address",
        )
        .bind(POOL_ACCOUNT)
        .bind(to_i64(minimum.units(), "payout threshold")?)
        .fetch_all(self.wr())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(EligibleBalance {
                    address: MinerAddress::from_trusted(row.get("address")),
                    balance: Amount::new(to_u64(row.get("balance"), "balance")?),
                })
            })
            .collect()
    }
}
