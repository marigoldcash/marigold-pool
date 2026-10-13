//! Paying what is owed, in batches, never twice.
//!
//! The rules that keep a payment from being made twice or lost, in the order a
//! payment goes through them:
//!
//! 1. A payout is written down as `pending` and taken off the miner's balance in
//!    one database transaction, before anything is sent. A `pending` payout has
//!    certainly not been sent.
//! 2. The wallet is told to pay. Just before it broadcasts the transaction that
//!    carries the payments, it hands that transaction's id and change address
//!    to the pool, which writes them down and marks the payouts `sent`. If that
//!    cannot be written, the transaction is not broadcast.
//! 3. Once the node shows the transaction in a block, the payouts are
//!    `confirmed` and counted as paid.
//!
//! After a crash at any moment, a payout is in exactly one of these states, and
//! each has one safe way out: `pending` was never sent, so it is cancelled and
//! the money goes back to the miner's balance; `sent` is looked up on the node
//! by its transaction id, and is confirmed if it is there, or cancelled if it
//! is not and enough time has passed that it never will be. While any `sent`
//! payout is unresolved, no new payment is made.

mod engine;
mod schedule;

pub use engine::{
    CycleSummary, DEFAULT_BATCH_SIZE, DEFAULT_KEEP_BACK, DEFAULT_MIN_PAYOUT_COINS, DEFAULT_SETTLE, PayoutError, PayoutSettings,
    describe, recover, run_cycle,
};
pub use schedule::{Schedule, run_scheduler};
