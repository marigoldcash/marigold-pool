//! Paying miners, in terms that do not name a chain. The chain's backend
//! implements `Payer` over its own wallet.

use std::path::PathBuf;
use std::sync::Arc;

/// One payment: an address and an amount in the chain's smallest unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayOutput {
    pub address: String,
    pub amount: u64,
}

/// Where the node has seen a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxSighting {
    /// Nowhere: it was never broadcast, or it was dropped without being confirmed.
    Unknown,
    /// Waiting in the node's pool of unconfirmed transactions.
    InMempool,
    /// In a block.
    InChain,
}

/// Why a payment step could not be done, as a sentence for the operator.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PayError(pub String);

/// The pool tells its wallet to stop here. Called with the id of the
/// transaction that carries the payments, and the change address it will leave
/// change at, **before** that transaction is broadcast. If it fails, the
/// transaction must not be broadcast.
///
/// This is the pool's whole defence against paying twice: with the id already
/// written down, a crash at any later moment can be sorted out by asking the
/// node whether that transaction exists.
#[async_trait::async_trait]
pub trait SubmitGuard: Send + Sync {
    async fn before_broadcast(&self, txid: &str, change_address: &str) -> Result<(), PayError>;
}

#[async_trait::async_trait]
pub trait Payer: Send + Sync {
    /// Pay `outputs` in one go, and return the id of the transaction that
    /// carries them. The wallet may first send other transactions of its own
    /// (to gather many small coins into a few big ones); those move only the
    /// pool's own money. `guard` is called exactly once, before the
    /// transaction that carries the payments is broadcast.
    async fn pay(&self, outputs: &[PayOutput], guard: &dyn SubmitGuard) -> Result<String, PayError>;

    /// Where the node has seen a transaction. `watch` are addresses the
    /// transaction's outputs went to (its recipients and its change address),
    /// to look for it among the confirmed coins. This must be an error, not
    /// `Unknown`, whenever the node cannot be trusted to know: unreachable, or
    /// still catching up with the network.
    async fn sighting(&self, txid: &str, watch: &[String]) -> Result<TxSighting, PayError>;

    /// What the wallet can spend right now, in the chain's smallest unit.
    async fn spendable(&self) -> Result<u64, PayError>;

    /// Whether this wallet controls `address`: the one `init` printed, or its
    /// current receive or change address. The pool checks this at start, because
    /// rewards paid to an address the wallet does not control can never be paid out.
    async fn owns_address(&self, address: &str) -> Result<bool, PayError>;
}

pub type SharedPayer = Arc<dyn Payer>;

/// The password that protects the pool wallet. Never printed.
#[derive(Clone)]
pub struct WalletSecret(String);

impl WalletSecret {
    pub fn new(secret: String) -> Self {
        WalletSecret(secret)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for WalletSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WalletSecret(..)")
    }
}

/// Where the pool wallet lives and how to reach the node it works through.
#[derive(Debug, Clone)]
pub struct WalletSettings {
    /// The network id, such as "testnet-10".
    pub network: String,
    /// The node's wRPC address, such as `ws://127.0.0.1:27210`.
    pub wrpc_url: String,
    /// The pool's data directory; the wallet file is kept in a folder inside it.
    pub data_dir: PathBuf,
    pub secret: WalletSecret,
}

/// A wallet that was just made.
#[derive(Clone)]
pub struct WalletCreated {
    /// The address block rewards should be paid to.
    pub address: String,
    /// The words that recover the wallet. Shown once and never stored by the pool.
    pub recovery_phrase: String,
}

impl std::fmt::Debug for WalletCreated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletCreated").field("address", &self.address).field("recovery_phrase", &"..").finish()
    }
}
