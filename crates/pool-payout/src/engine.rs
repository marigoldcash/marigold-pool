use pool_chain::{Chain, PayError, PayOutput, Payer, SubmitGuard, TxSighting};
use pool_db::{Db, DbError, NewPayout, Payout, PayoutStatus};
use pool_domain::Amount;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// Miners paid by one transaction.
pub const DEFAULT_BATCH_SIZE: usize = 50;

/// What a miner must be owed, in whole coins, before they are paid.
pub const DEFAULT_MIN_PAYOUT_COINS: u64 = 10;

/// How long a payment that the node has never heard of is given before it is
/// written off as never sent. It is the margin against a transaction that was
/// accepted at the very moment the pool stopped.
pub const DEFAULT_SETTLE: Duration = Duration::from_secs(10 * 60);

const LOCK_NAME: &str = "payout";
const LOCK_TTL_MS: u64 = 30 * 60 * 1000;
const CONFIRM_POLL: Duration = Duration::from_millis(500);

const NOTE_NEVER_SENT: &str =
    "The pool stopped before this payment was sent, so it was cancelled and the money returned to the miner's balance.";
const NOTE_VANISHED: &str = "The network never saw this payment, so it was cancelled and the money returned to the miner's balance.";

#[derive(Debug, Clone)]
pub struct PayoutSettings {
    /// A miner is paid only once they are owed at least this much.
    pub min_payout: Amount,
    /// Miners per transaction.
    pub batch_size: usize,
    /// How long, after sending, to wait for the network to confirm.
    pub confirm_wait: Duration,
    /// How long a payment the node has never heard of is given before it is cancelled.
    pub settle: Duration,
}

impl PayoutSettings {
    pub fn new(min_payout: Amount) -> Self {
        PayoutSettings { min_payout, batch_size: DEFAULT_BATCH_SIZE, confirm_wait: Duration::from_secs(60), settle: DEFAULT_SETTLE }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PayoutError {
    #[error("{0}")]
    Db(#[from] DbError),
    #[error("{0}")]
    Pay(#[from] PayError),
    #[error("Another payout run is already going on this database, so this one did not start.")]
    Busy,
}

/// What a payout run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CycleSummary {
    /// Payments from an earlier run that never went out, cancelled and returned to balances.
    pub cancelled: u64,
    /// Payments from an earlier run found to have gone through.
    pub confirmed_earlier: u64,
    /// Payments from an earlier run still waiting on the network. While there are any, nothing new is sent.
    pub waiting_on_network: u64,
    /// Nobody was owed enough to be paid.
    pub nobody_owed: bool,
    /// Some miners are owed something, but less than the minimum.
    pub someone_owed_less: bool,
    pub batches_paid: u64,
    pub miners_paid: u64,
    pub paid: Amount,
    /// Of the payments sent in this run, how many the network had confirmed before it finished.
    pub confirmed_now: u64,
    /// Miners left unpaid because the wallet did not hold enough.
    pub skipped_miners: u64,
    pub wallet_had: Amount,
    /// The last batch did not go out; the money is back in the balances.
    pub failed: Option<String>,
    /// A batch may or may not have gone out; it will be checked at the next run.
    pub unsure: Option<String>,
}

/// Writes the payment down at the moment the wallet is about to broadcast it.
struct BatchGuard<'a> {
    db: &'a Db,
    ids: &'a [i64],
    now_ms: u64,
    called: AtomicBool,
}

#[async_trait::async_trait]
impl SubmitGuard for BatchGuard<'_> {
    async fn before_broadcast(&self, txid: &str, change_address: &str) -> Result<(), PayError> {
        self.db
            .mark_payouts_sent(self.ids, txid, Some(change_address), self.now_ms)
            .await
            .map_err(|e| PayError(format!("The payment could not be written down before sending, so it was not sent: {e}")))?;
        self.called.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Recovery {
    cancelled: u64,
    confirmed: u64,
    waiting: u64,
}

/// Sort out payouts left over from an earlier run (or an earlier moment of this
/// one). Run it at start-up and before every payout run.
///
/// * `pending` was never sent: cancelled, money back on the balance.
/// * `sent` is looked up on the node by its transaction id. In a block:
///   confirmed. Seen waiting: left alone. Not seen at all: cancelled, but only
///   once `settle` has passed since it was written down.
///
/// A node that cannot answer is an error, never a reason to cancel.
pub async fn recover(db: &Db, payer: &dyn Payer, settings: &PayoutSettings, now_ms: u64) -> Result<(u64, u64, u64), PayoutError> {
    let r = recover_inner(db, payer, settings, now_ms, true).await?;
    Ok((r.cancelled, r.confirmed, r.waiting))
}

async fn recover_inner(
    db: &Db,
    payer: &dyn Payer,
    settings: &PayoutSettings,
    now_ms: u64,
    may_cancel: bool,
) -> Result<Recovery, PayoutError> {
    let mut out = Recovery::default();

    if may_cancel {
        let pending = db.payouts_with_status(PayoutStatus::Pending).await?;
        if !pending.is_empty() {
            let ids: Vec<i64> = pending.iter().map(|p| p.id).collect();
            db.fail_payouts(&ids, NOTE_NEVER_SENT, now_ms).await?;
            out.cancelled += ids.len() as u64;
            warn!("{} payment(s) written down but never sent were cancelled", ids.len());
        }
    }

    let mut by_tx: BTreeMap<String, Vec<Payout>> = BTreeMap::new();
    for payout in db.payouts_with_status(PayoutStatus::Sent).await? {
        if let Some(txid) = payout.txid.clone() {
            by_tx.entry(txid).or_default().push(payout);
        }
    }
    for (txid, payouts) in by_tx {
        let ids: Vec<i64> = payouts.iter().map(|p| p.id).collect();
        let mut watch: BTreeSet<String> = payouts.iter().map(|p| p.address.to_string()).collect();
        watch.extend(payouts.iter().filter_map(|p| p.watch_address.clone()));
        let watch: Vec<String> = watch.into_iter().collect();

        match payer.sighting(&txid, &watch).await? {
            TxSighting::InChain => {
                db.confirm_payouts(&ids, now_ms).await?;
                out.confirmed += ids.len() as u64;
                info!("payment {txid} is confirmed ({} miner(s))", ids.len());
            }
            TxSighting::InMempool => out.waiting += ids.len() as u64,
            TxSighting::Unknown => {
                let sent_at = payouts.iter().map(|p| p.updated_ts_ms).max().unwrap_or(now_ms);
                let settled = now_ms.saturating_sub(sent_at) >= settings.settle.as_millis() as u64;
                if may_cancel && settled {
                    db.fail_payouts(&ids, NOTE_VANISHED, now_ms).await?;
                    out.cancelled += ids.len() as u64;
                    warn!("payment {txid} was never seen by the network and was cancelled ({} miner(s))", ids.len());
                } else {
                    out.waiting += ids.len() as u64;
                }
            }
        }
    }
    Ok(out)
}

enum BatchResult {
    Sent { total: u64, miners: u64 },
    Skipped,
    Failed(String),
    Unsure(String),
}

async fn send_batch(db: &Db, payer: &dyn Payer, batch: &[pool_db::EligibleBalance], now_ms: u64) -> Result<BatchResult, PayoutError> {
    let mut new = Vec::with_capacity(batch.len());
    for owed in batch {
        let n = db.payout_count(&owed.address).await? + 1;
        new.push(NewPayout {
            idempotency_key: format!("payout:{}:{n}", owed.address),
            address: owed.address.clone(),
            amount: owed.balance,
        });
    }
    let rows = match db.create_pending_payouts(&new, now_ms).await {
        Ok(rows) => rows,
        Err(DbError::InsufficientBalance { .. } | DbError::DuplicatePayoutKey(_)) => {
            debug!("a batch could not be written down because the books changed under it; skipped");
            return Ok(BatchResult::Skipped);
        }
        Err(other) => return Err(other.into()),
    };
    let ids: Vec<i64> = rows.iter().map(|p| p.id).collect();
    let outputs: Vec<PayOutput> =
        rows.iter().map(|p| PayOutput { address: p.address.to_string(), amount: p.amount.units() }).collect();
    let total: u64 = outputs.iter().map(|o| o.amount).sum();

    let guard = BatchGuard { db, ids: &ids, now_ms, called: AtomicBool::new(false) };
    match payer.pay(&outputs, &guard).await {
        Ok(txid) => {
            if !guard.called.load(Ordering::SeqCst) {
                // The wallet did not tell us before sending. Write it down now:
                // late, but the next run can still find the payment.
                error!("the wallet sent payment {txid} without telling the pool first; recording it now");
                db.mark_payouts_sent(&ids, &txid, None, now_ms).await?;
            }
            Ok(BatchResult::Sent { total, miners: outputs.len() as u64 })
        }
        Err(why) if guard.called.load(Ordering::SeqCst) => {
            warn!("a payment may or may not have gone out: {why}");
            Ok(BatchResult::Unsure(why.to_string()))
        }
        Err(why) => {
            db.fail_payouts(&ids, &format!("The payment could not be sent: {why}"), now_ms).await?;
            Ok(BatchResult::Failed(why.to_string()))
        }
    }
}

/// Wait up to `settings.confirm_wait` for the network to confirm what was just
/// sent. Never cancels anything: a payment that is not there yet is left for
/// the next run to look at.
async fn confirm_sent(db: &Db, payer: &dyn Payer, settings: &PayoutSettings, now_ms: u64) -> u64 {
    let deadline = Instant::now() + settings.confirm_wait;
    let mut confirmed = 0;
    loop {
        match recover_inner(db, payer, settings, now_ms, false).await {
            Ok(step) => {
                confirmed += step.confirmed;
                if step.waiting == 0 || Instant::now() >= deadline {
                    return confirmed;
                }
            }
            Err(why) => {
                warn!("could not check whether the payments were confirmed: {why}");
                return confirmed;
            }
        }
        tokio::time::sleep(CONFIRM_POLL).await;
    }
}

/// One payout run: sort out anything left over, then pay every miner who is
/// owed at least the minimum, in batches. Two runs on the same database cannot
/// overlap.
pub async fn run_cycle(db: &Db, payer: &dyn Payer, settings: &PayoutSettings, now_ms: u64) -> Result<CycleSummary, PayoutError> {
    let owner = format!("pid{}-{now_ms}", std::process::id());
    if !db.try_lock(LOCK_NAME, &owner, now_ms, LOCK_TTL_MS).await? {
        return Err(PayoutError::Busy);
    }
    let result = cycle(db, payer, settings, now_ms).await;
    if let Err(why) = db.unlock(LOCK_NAME, &owner).await {
        warn!("could not give back the payout lock (it runs out by itself): {why}");
    }
    result
}

async fn cycle(db: &Db, payer: &dyn Payer, settings: &PayoutSettings, now_ms: u64) -> Result<CycleSummary, PayoutError> {
    let mut summary = CycleSummary::default();

    let earlier = recover_inner(db, payer, settings, now_ms, true).await?;
    summary.cancelled = earlier.cancelled;
    summary.confirmed_earlier = earlier.confirmed;
    summary.waiting_on_network = earlier.waiting;
    if earlier.waiting > 0 {
        return Ok(summary);
    }

    let owed = db.balances_eligible(settings.min_payout).await?;
    if owed.is_empty() {
        summary.nobody_owed = true;
        summary.someone_owed_less = !db.balances_eligible(Amount::ZERO).await?.is_empty();
        return Ok(summary);
    }

    let mut spendable = payer.spendable().await?;
    summary.wallet_had = Amount::new(spendable);
    let mut sent_any = false;
    for batch in owed.chunks(settings.batch_size.max(1)) {
        let total: u64 = batch.iter().map(|b| b.balance.units()).sum();
        if total > spendable {
            summary.skipped_miners += batch.len() as u64;
            continue;
        }
        match send_batch(db, payer, batch, now_ms).await? {
            BatchResult::Sent { total, miners, .. } => {
                spendable = spendable.saturating_sub(total);
                summary.batches_paid += 1;
                summary.miners_paid += miners;
                summary.paid = summary.paid.checked_add(Amount::new(total)).unwrap_or(Amount::new(u64::MAX));
                sent_any = true;
            }
            BatchResult::Skipped => continue,
            BatchResult::Failed(why) => {
                summary.failed = Some(why);
                break;
            }
            BatchResult::Unsure(why) => {
                summary.unsure = Some(why);
                break;
            }
        }
    }

    if sent_any {
        summary.confirmed_now = confirm_sent(db, payer, settings, now_ms).await;
    }
    Ok(summary)
}

/// What a run did, in plain words, for whoever ran it.
pub fn describe<C: Chain>(summary: &CycleSummary, min_payout: Amount) -> String {
    let mut lines = Vec::new();
    if summary.cancelled > 0 {
        lines.push(format!(
            "{} payment(s) from an earlier run never went out. They were cancelled and the money is back in the miners' balances.",
            summary.cancelled
        ));
    }
    if summary.confirmed_earlier > 0 {
        lines.push(format!(
            "{} payment(s) from an earlier run had gone through and are now marked as paid.",
            summary.confirmed_earlier
        ));
    }
    if summary.waiting_on_network > 0 {
        lines.push(format!(
            "{} payment(s) from an earlier run are still waiting for the network to confirm them, so nothing new was sent. Try again in a few minutes.",
            summary.waiting_on_network
        ));
        return lines.join("\n");
    }
    if summary.nobody_owed {
        if summary.someone_owed_less {
            lines.push(format!(
                "Nobody is owed {} or more yet, so there is nothing to pay. Some miners are owed smaller amounts and will be paid once they reach it.",
                min_payout.display::<C>()
            ));
        } else {
            lines.push("Nobody is owed anything yet, so there is nothing to pay.".to_owned());
        }
        return lines.join("\n");
    }
    if summary.miners_paid > 0 {
        lines.push(format!(
            "Paid {} miner(s) {} in {} payment(s). The network has confirmed {} of them so far; the rest will be confirmed shortly.",
            summary.miners_paid,
            summary.paid.display::<C>(),
            summary.batches_paid,
            summary.confirmed_now
        ));
    }
    if summary.skipped_miners > 0 {
        lines.push(format!(
            "The pool wallet holds {}, which is not enough to pay {} more miner(s). They were not paid and will be tried again at the next run.",
            summary.wallet_had.display::<C>(),
            summary.skipped_miners
        ));
    }
    if let Some(why) = &summary.failed {
        lines.push(format!(
            "The last payment did not go through: {why} The money is back in the miners' balances and will be tried again."
        ));
    }
    if let Some(why) = &summary.unsure {
        lines.push(format!(
            "A payment was sent but the pool could not confirm that it went through ({why}). It will be checked again at the next run, and nothing new will be sent until then."
        ));
    }
    if lines.is_empty() {
        lines.push("Nothing was paid.".to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pool_chain::testing::FakeChain;
    use pool_domain::{Account, MinerAddress};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ---- a payer that can break in every way a real one can ---------------

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Normal,
        /// Fails before telling the pool anything: nothing was sent.
        FailBeforeGuard,
        /// Tells the pool, then the process "dies". `broadcast` says whether the transaction got out first.
        DieAfterGuard {
            broadcast: bool,
        },
    }

    struct Sent {
        outputs: Vec<PayOutput>,
    }

    struct State {
        mode: Mode,
        spendable: u64,
        auto_mine: bool,
        sent: Vec<Sent>,
        seen: HashMap<String, TxSighting>,
        sighting_error: Option<String>,
        asked_about: Vec<(String, Vec<String>)>,
        batch_sizes: Vec<usize>,
        next: u64,
    }

    struct FakePayer(Mutex<State>);

    impl FakePayer {
        fn new() -> Self {
            FakePayer(Mutex::new(State {
                mode: Mode::Normal,
                spendable: u64::MAX,
                auto_mine: true,
                sent: vec![],
                seen: HashMap::new(),
                sighting_error: None,
                asked_about: vec![],
                batch_sizes: vec![],
                next: 1,
            }))
        }

        fn set_mode(&self, mode: Mode) {
            self.0.lock().unwrap().mode = mode;
        }

        fn set_spendable(&self, n: u64) {
            self.0.lock().unwrap().spendable = n;
        }

        fn set_auto_mine(&self, on: bool) {
            self.0.lock().unwrap().auto_mine = on;
        }

        fn fail_sightings(&self, why: Option<&str>) {
            self.0.lock().unwrap().sighting_error = why.map(str::to_owned);
        }

        /// A block arrives: everything waiting is now confirmed.
        fn mine(&self) {
            for seen in self.0.lock().unwrap().seen.values_mut() {
                *seen = TxSighting::InChain;
            }
        }

        fn sent_batches(&self) -> usize {
            self.0.lock().unwrap().sent.len()
        }

        fn money_sent(&self) -> u64 {
            self.0.lock().unwrap().sent.iter().flat_map(|s| s.outputs.iter()).map(|o| o.amount).sum()
        }

        fn batch_sizes(&self) -> Vec<usize> {
            self.0.lock().unwrap().batch_sizes.clone()
        }
    }

    #[async_trait::async_trait]
    impl Payer for FakePayer {
        async fn pay(&self, outputs: &[PayOutput], guard: &dyn SubmitGuard) -> Result<String, PayError> {
            let (mode, txid) = {
                let mut s = self.0.lock().unwrap();
                s.batch_sizes.push(outputs.len());
                let txid = format!("tx{}", s.next);
                s.next += 1;
                (s.mode, txid)
            };
            if mode == Mode::FailBeforeGuard {
                return Err(PayError("the wallet is out of coins".into()));
            }
            guard.before_broadcast(&txid, "fake:change0").await?;
            let mut s = self.0.lock().unwrap();
            let broadcast = !matches!(mode, Mode::DieAfterGuard { broadcast: false });
            if broadcast {
                let total: u64 = outputs.iter().map(|o| o.amount).sum();
                s.spendable = s.spendable.saturating_sub(total);
                s.sent.push(Sent { outputs: outputs.to_vec() });
                let state = if s.auto_mine { TxSighting::InChain } else { TxSighting::InMempool };
                s.seen.insert(txid.clone(), state);
            }
            if matches!(mode, Mode::DieAfterGuard { .. }) {
                return Err(PayError("the connection to the node was lost".into()));
            }
            Ok(txid)
        }

        async fn sighting(&self, txid: &str, watch: &[String]) -> Result<TxSighting, PayError> {
            let mut s = self.0.lock().unwrap();
            if let Some(why) = &s.sighting_error {
                return Err(PayError(why.clone()));
            }
            s.asked_about.push((txid.to_owned(), watch.to_vec()));
            Ok(s.seen.get(txid).copied().unwrap_or(TxSighting::Unknown))
        }

        async fn spendable(&self) -> Result<u64, PayError> {
            Ok(self.0.lock().unwrap().spendable)
        }
    }

    // ---- fixtures ----------------------------------------------------------

    fn who(n: usize) -> MinerAddress {
        MinerAddress::from_trusted(format!("fake:m{n:03}"))
    }

    const COIN: u64 = 100_000_000;

    /// Credit `owed` to each miner through a real reward and split, the way the books really fill up.
    async fn owe(db: &Db, owed: &[(usize, u64)]) {
        for (i, (miner, units)) in owed.iter().enumerate() {
            let key = format!("reward{miner}-{i}");
            db.record_reward_if_new(&key, Amount::new(*units), 1, 0).await.unwrap();
            db.apply_allocation(&key, &[(Account::Miner(who(*miner)), Amount::new(*units))], 1).await.unwrap();
        }
    }

    async fn balance(db: &Db, miner: usize) -> (u64, u64) {
        db.balance_of(&Account::Miner(who(miner))).await.unwrap().map(|b| (b.balance.units(), b.total_paid.units())).unwrap_or((0, 0))
    }

    fn settings() -> PayoutSettings {
        PayoutSettings { min_payout: Amount::new(10 * COIN), batch_size: 50, confirm_wait: Duration::ZERO, settle: Duration::ZERO }
    }

    const NOW: u64 = 1_000_000;

    // ---- the ordinary run ---------------------------------------------------

    #[tokio::test]
    async fn only_miners_at_or_over_the_threshold_are_paid_and_the_rest_keep_their_balance() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 10 * COIN - 1), (2, 10 * COIN), (3, 25 * COIN)]).await;
        let payer = FakePayer::new();

        let summary = run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        assert_eq!((summary.miners_paid, summary.batches_paid, summary.paid), (2, 1, Amount::new(35 * COIN)));
        assert_eq!(balance(&db, 1).await, (10 * COIN - 1, 0), "one petal short: not paid, balance kept");
        assert_eq!(balance(&db, 2).await, (0, 10 * COIN));
        assert_eq!(balance(&db, 3).await, (0, 25 * COIN));
        assert_eq!(payer.money_sent(), 35 * COIN);
    }

    #[tokio::test]
    async fn a_hundred_and_twenty_miners_go_out_in_batches_of_fifty() {
        let db = Db::open_in_memory().await.unwrap();
        let owed: Vec<(usize, u64)> = (0..120).map(|m| (m, 10 * COIN)).collect();
        owe(&db, &owed).await;
        let payer = FakePayer::new();

        let summary = run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        assert_eq!(payer.batch_sizes(), [50, 50, 20]);
        assert_eq!((summary.batches_paid, summary.miners_paid, summary.confirmed_now), (3, 120, 120));
        assert_eq!(payer.money_sent(), 120 * 10 * COIN);
        assert!(db.balances_eligible(Amount::ZERO).await.unwrap().is_empty(), "nobody is left owed");
        assert_eq!(db.payouts_with_status(PayoutStatus::Confirmed).await.unwrap().len(), 120);
        assert_eq!(balance(&db, 119).await, (0, 10 * COIN));
    }

    #[tokio::test]
    async fn running_again_pays_nobody_a_second_time() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN)]).await;
        let payer = FakePayer::new();
        run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        let again = run_cycle(&db, &payer, &settings(), NOW + 1).await.unwrap();
        assert!(again.nobody_owed);
        assert_eq!(payer.sent_batches(), 1);
        assert_eq!(payer.money_sent(), 20 * COIN);
    }

    #[tokio::test]
    async fn the_payments_carry_each_miners_exact_balance() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 10 * COIN + 7), (2, 33 * COIN + 99)]).await;
        let payer = FakePayer::new();
        run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        let sent = &payer.0.lock().unwrap().sent;
        assert_eq!(
            sent[0].outputs,
            vec![
                PayOutput { address: "fake:m001".into(), amount: 10 * COIN + 7 },
                PayOutput { address: "fake:m002".into(), amount: 33 * COIN + 99 },
            ]
        );
    }

    // ---- nothing to do ------------------------------------------------------

    #[tokio::test]
    async fn an_empty_database_says_nobody_is_owed_anything() {
        let db = Db::open_in_memory().await.unwrap();
        let payer = FakePayer::new();
        let summary = run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        assert!(summary.nobody_owed && !summary.someone_owed_less);
        assert_eq!(describe::<FakeChain>(&summary, settings().min_payout), "Nobody is owed anything yet, so there is nothing to pay.");
        assert_eq!(payer.sent_batches(), 0);
    }

    #[tokio::test]
    async fn small_balances_are_explained() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, COIN)]).await;
        let summary = run_cycle(&db, &FakePayer::new(), &settings(), NOW).await.unwrap();
        assert!(summary.nobody_owed && summary.someone_owed_less);
        assert_eq!(
            describe::<FakeChain>(&summary, settings().min_payout),
            "Nobody is owed 10.00 TST or more yet, so there is nothing to pay. Some miners are owed smaller amounts and will be paid once they reach it."
        );
    }

    // ---- crashes ------------------------------------------------------------

    #[tokio::test]
    async fn a_crash_after_writing_payouts_down_but_before_sending_neither_double_pays_nor_loses_the_money() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN), (2, 30 * COIN), (3, 40 * COIN)]).await;
        // The previous run wrote the payouts down (balances taken off) and died: nothing sent, no transaction id.
        let items: Vec<NewPayout> = (1..=3)
            .map(|m| NewPayout {
                idempotency_key: format!("payout:{}:1", who(m)),
                address: who(m),
                amount: Amount::new((10 + 10 * m as u64) * COIN),
            })
            .collect();
        db.create_pending_payouts(&items, NOW - 5).await.unwrap();
        assert_eq!(balance(&db, 1).await.0, 0, "the balance was taken when the payout was written down");

        let payer = FakePayer::new();
        let summary = run_cycle(&db, &payer, &settings(), NOW).await.unwrap();

        assert_eq!(summary.cancelled, 3);
        assert_eq!(payer.sent_batches(), 1, "paid once, in the fresh run");
        assert_eq!(payer.money_sent(), 90 * COIN, "exactly what was owed, no more");
        for (m, units) in [(1, 20 * COIN), (2, 30 * COIN), (3, 40 * COIN)] {
            assert_eq!(balance(&db, m).await, (0, units), "miner {m}: nothing lost, nothing twice");
        }
        assert_eq!(db.payouts_with_status(PayoutStatus::Failed).await.unwrap().len(), 3);
        assert_eq!(db.payouts_with_status(PayoutStatus::Confirmed).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_crash_between_writing_the_transaction_id_down_and_broadcasting_it_is_sorted_out_by_the_node_not_knowing_it() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN), (2, 30 * COIN)]).await;
        let payer = FakePayer::new();
        payer.set_mode(Mode::DieAfterGuard { broadcast: false });

        let first = run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        assert!(first.unsure.is_some() && first.miners_paid == 0);
        assert_eq!(payer.sent_batches(), 0, "it never got out");
        assert_eq!(
            db.payouts_with_status(PayoutStatus::Sent).await.unwrap().len(),
            2,
            "written down as sent, with its transaction id"
        );
        assert_eq!(balance(&db, 1).await.0, 0);

        // The pool restarts. The node has never heard of that transaction, and enough time has passed.
        payer.set_mode(Mode::Normal);
        let second = run_cycle(&db, &payer, &settings(), NOW + 1).await.unwrap();
        assert_eq!(second.cancelled, 2);
        assert_eq!(payer.sent_batches(), 1);
        assert_eq!(payer.money_sent(), 50 * COIN);
        assert_eq!(balance(&db, 1).await, (0, 20 * COIN));
        assert_eq!(balance(&db, 2).await, (0, 30 * COIN));
    }

    #[tokio::test]
    async fn the_node_is_given_time_before_a_payment_it_does_not_know_is_written_off() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN)]).await;
        let payer = FakePayer::new();
        payer.set_mode(Mode::DieAfterGuard { broadcast: false });
        run_cycle(&db, &payer, &settings(), NOW).await.unwrap();

        payer.set_mode(Mode::Normal);
        let mut patient = settings();
        patient.settle = Duration::from_secs(600);
        let soon = run_cycle(&db, &payer, &patient, NOW + 1_000).await.unwrap();
        assert_eq!((soon.cancelled, soon.waiting_on_network, soon.miners_paid), (0, 1, 0), "10 minutes have not passed: wait");
        assert_eq!(payer.sent_batches(), 0, "and send nothing new meanwhile");

        let later = run_cycle(&db, &payer, &patient, NOW + 601_000).await.unwrap();
        assert_eq!((later.cancelled, later.miners_paid), (1, 1));
        assert_eq!(payer.money_sent(), 20 * COIN);
    }

    #[tokio::test]
    async fn a_crash_after_the_broadcast_confirms_the_payment_instead_of_sending_it_again() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN), (2, 30 * COIN)]).await;
        let payer = FakePayer::new();
        payer.set_mode(Mode::DieAfterGuard { broadcast: true });
        let first = run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        assert!(first.unsure.is_some());
        assert_eq!(payer.sent_batches(), 1, "the transaction did get out");

        payer.set_mode(Mode::Normal);
        let second = run_cycle(&db, &payer, &settings(), NOW + 1).await.unwrap();
        assert_eq!((second.confirmed_earlier, second.cancelled, second.miners_paid), (2, 0, 0));
        assert_eq!(payer.sent_batches(), 1, "not sent a second time");
        assert_eq!(payer.money_sent(), 50 * COIN);
        assert_eq!(balance(&db, 1).await, (0, 20 * COIN));
        assert_eq!(balance(&db, 2).await, (0, 30 * COIN));
    }

    #[tokio::test]
    async fn the_node_is_asked_about_the_transaction_where_its_outputs_would_be() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN), (2, 30 * COIN)]).await;
        let payer = FakePayer::new();
        payer.set_mode(Mode::DieAfterGuard { broadcast: true });
        run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        payer.set_mode(Mode::Normal);
        run_cycle(&db, &payer, &settings(), NOW + 1).await.unwrap();

        let asked = payer.0.lock().unwrap().asked_about.clone();
        let (txid, watch) = asked.iter().find(|(t, _)| t == "tx1").expect("the node was asked about tx1");
        assert_eq!(txid, "tx1");
        assert_eq!(
            watch,
            &["fake:change0".to_string(), "fake:m001".to_string(), "fake:m002".to_string()],
            "the change address and both recipients"
        );
    }

    #[tokio::test]
    async fn while_a_payment_is_still_waiting_on_the_network_nothing_new_is_sent() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN)]).await;
        let payer = FakePayer::new();
        payer.set_auto_mine(false); // sent, but no block yet
        let first = run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        assert_eq!((first.miners_paid, first.confirmed_now), (1, 0));
        assert_eq!(db.payouts_with_status(PayoutStatus::Sent).await.unwrap().len(), 1);

        owe(&db, &[(2, 40 * COIN)]).await; // someone else becomes owed in the meantime
        let second = run_cycle(&db, &payer, &settings(), NOW + 1).await.unwrap();
        assert_eq!((second.waiting_on_network, second.miners_paid), (1, 0));
        assert_eq!(payer.sent_batches(), 1, "the newly owed miner waits");
        assert!(describe::<FakeChain>(&second, settings().min_payout).contains("still waiting for the network"));

        payer.mine();
        payer.set_auto_mine(true); // and from now on new payments are mined at once
        let third = run_cycle(&db, &payer, &settings(), NOW + 2).await.unwrap();
        assert_eq!((third.confirmed_earlier, third.miners_paid), (1, 1));
        assert_eq!(balance(&db, 1).await, (0, 20 * COIN));
        assert_eq!(balance(&db, 2).await, (0, 40 * COIN));
    }

    #[tokio::test]
    async fn a_node_that_cannot_answer_cancels_nothing_and_stops_the_run() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN)]).await;
        let payer = FakePayer::new();
        payer.set_mode(Mode::DieAfterGuard { broadcast: true });
        run_cycle(&db, &payer, &settings(), NOW).await.unwrap();

        payer.set_mode(Mode::Normal);
        payer.fail_sightings(Some("the node is still catching up"));
        let error = run_cycle(&db, &payer, &settings(), NOW + 1).await.unwrap_err();
        assert!(matches!(error, PayoutError::Pay(_)), "{error}");
        assert_eq!(db.payouts_with_status(PayoutStatus::Sent).await.unwrap().len(), 1, "left exactly as it was");
        assert_eq!(payer.sent_batches(), 1);

        payer.fail_sightings(None);
        let ok = run_cycle(&db, &payer, &settings(), NOW + 2).await.unwrap();
        assert_eq!(ok.confirmed_earlier, 1);
    }

    // ---- failures -----------------------------------------------------------

    #[tokio::test]
    async fn a_payment_that_fails_before_it_is_sent_puts_the_money_back_and_stops_the_run() {
        let db = Db::open_in_memory().await.unwrap();
        let owed: Vec<(usize, u64)> = (0..120).map(|m| (m, 10 * COIN)).collect();
        owe(&db, &owed).await;
        let payer = FakePayer::new();
        payer.set_mode(Mode::FailBeforeGuard);

        let summary = run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        assert_eq!(summary.failed.as_deref(), Some("the wallet is out of coins"));
        assert_eq!(payer.batch_sizes(), [50], "the run stopped at the first failure");
        assert_eq!(summary.miners_paid, 0);
        assert_eq!(db.balances_eligible(Amount::new(10 * COIN)).await.unwrap().len(), 120, "every balance is back");
        assert_eq!(db.payouts_with_status(PayoutStatus::Failed).await.unwrap().len(), 50);
        assert!(describe::<FakeChain>(&summary, settings().min_payout).contains("did not go through"));

        payer.set_mode(Mode::Normal);
        let retry = run_cycle(&db, &payer, &settings(), NOW + 1).await.unwrap();
        assert_eq!(retry.miners_paid, 120, "the next run pays them all");
        assert_eq!(payer.money_sent(), 120 * 10 * COIN);
    }

    #[tokio::test]
    async fn a_wallet_that_holds_too_little_pays_what_it_can_and_says_who_waits() {
        let db = Db::open_in_memory().await.unwrap();
        let owed: Vec<(usize, u64)> = (0..120).map(|m| (m, 10 * COIN)).collect();
        owe(&db, &owed).await;
        let payer = FakePayer::new();
        payer.set_spendable(50 * 10 * COIN + 5); // enough for the first batch of fifty and not the rest

        let summary = run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        assert_eq!((summary.batches_paid, summary.miners_paid, summary.skipped_miners), (1, 50, 70));
        assert_eq!(payer.batch_sizes(), [50], "the others were not even written down");
        assert_eq!(db.balances_eligible(Amount::new(10 * COIN)).await.unwrap().len(), 70, "the 70 still have their balance");
        let text = describe::<FakeChain>(&summary, settings().min_payout);
        assert!(text.contains("not enough to pay 70 more miner(s)"), "{text}");
    }

    // ---- two at once --------------------------------------------------------

    #[tokio::test]
    async fn a_second_run_cannot_start_while_one_is_going() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN)]).await;
        assert!(db.try_lock("payout", "somebody-else", NOW, 60_000).await.unwrap());
        let payer = FakePayer::new();
        assert!(matches!(run_cycle(&db, &payer, &settings(), NOW + 1).await, Err(PayoutError::Busy)));
        assert_eq!(payer.sent_batches(), 0);

        db.unlock("payout", "somebody-else").await.unwrap();
        assert_eq!(run_cycle(&db, &payer, &settings(), NOW + 2).await.unwrap().miners_paid, 1);
    }

    #[tokio::test]
    async fn the_lock_is_given_back_even_when_the_run_fails() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 20 * COIN)]).await;
        let payer = FakePayer::new();
        payer.set_mode(Mode::DieAfterGuard { broadcast: true });
        run_cycle(&db, &payer, &settings(), NOW).await.unwrap();
        payer.fail_sightings(Some("down"));
        assert!(run_cycle(&db, &payer, &settings(), NOW + 1).await.is_err());
        payer.fail_sightings(None);
        assert!(run_cycle(&db, &payer, &settings(), NOW + 2).await.is_ok(), "not stuck behind a stale lock");
    }

    #[tokio::test]
    async fn the_report_after_a_successful_run_says_who_was_paid_and_what_is_confirmed() {
        let db = Db::open_in_memory().await.unwrap();
        owe(&db, &[(1, 12 * COIN + 34_000_000), (2, 20 * COIN)]).await;
        let summary = run_cycle(&db, &FakePayer::new(), &settings(), NOW).await.unwrap();
        assert_eq!(
            describe::<FakeChain>(&summary, settings().min_payout),
            "Paid 2 miner(s) 32.34 TST in 1 payment(s). The network has confirmed 2 of them so far; the rest will be confirmed shortly."
        );
    }
}
