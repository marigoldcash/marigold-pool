//! The pool wallet on Marigold: a plain wallet account that block rewards are
//! paid to and miners are paid from.
//!
//! The payment loop is `Account::send` written out by hand, with one addition.
//! The wallet's own send signs and broadcasts each transaction as it goes and
//! only tells the caller the ids afterwards. Here, the id of the transaction
//! that carries the payments is handed to the pool's guard before that
//! transaction is broadcast, so the pool can write it down first.

use crate::{PayError, PayOutput, Payer, SharedPayer, SubmitGuard, TxSighting, WalletCreated, WalletSettings};
use futures::TryStreamExt;
use kaspa_addresses::Address;
use kaspa_hashes::Hash;
use kaspa_wallet_core::prelude::*;
use kaspa_wallet_core::rpc::Rpc;
use kaspa_wallet_core::storage::keydata::PrvKeyDataVariantKind;
use kaspa_wallet_core::tx::{Generator, GeneratorSettings, Signer};
use kaspa_wrpc_client::prelude::{ConnectOptions, ConnectStrategy, NetworkId};
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Once};
use std::time::Duration;
use workflow_core::abortable::Abortable;

/// The wallet's file name inside the pool's wallet folder.
const WALLET_FILENAME: &str = "marigold-pool";

/// How long a freshly opened wallet gets to learn its balance before the pool
/// carries on (it then reports itself as not ready until it has).
const BALANCE_WAIT: Duration = Duration::from_secs(120);

fn problem(what: &str, error: impl std::fmt::Display) -> PayError {
    PayError(format!("The pool wallet could not {what}: {error}"))
}

/// Point the wallet library at a folder inside the pool's data directory. The
/// library keeps this in a process-wide setting and must be told before it does
/// anything else, which is why this is done once, first.
fn use_wallet_folder(data_dir: &Path) -> Result<(), PayError> {
    static SET: Once = Once::new();
    let folder = data_dir.join("wallet");
    let mut outcome = Ok(());
    SET.call_once(|| {
        // SAFETY: the library documents the setting as not thread-safe and as
        // needing to be set before any other wallet use. `Once` makes this the
        // only write, and every path into the wallet in this crate calls this
        // function before it creates a `Wallet`, so nothing reads the setting
        // earlier or at the same time.
        outcome = unsafe { kaspa_wallet_core::storage::local::set_default_storage_folder(folder.to_string_lossy().into_owned()) }
            .map_err(|e| problem("use its folder", e));
    });
    outcome
}

async fn connect_wallet(settings: &WalletSettings) -> Result<Arc<Wallet>, PayError> {
    use_wallet_folder(&settings.data_dir)?;
    let network_id = NetworkId::from_str(&settings.network).map_err(|e| problem("understand the network name", e))?;
    let client = Arc::new(
        KaspaRpcClient::new_with_args(WrpcEncoding::Borsh, Some(&settings.wrpc_url), None, Some(network_id), None)
            .map_err(|e| problem("prepare a connection to the node", e))?,
    );
    let rpc = Rpc::new(client.clone(), client.ctl().clone());
    let store = Wallet::local_store().map_err(|e| problem("open its storage", e))?;
    let wallet = Arc::new(Wallet::try_with_rpc(Some(rpc), store, Some(network_id)).map_err(|e| problem("start", e))?);
    // Fail at once on a wrong address or a node that is down, instead of
    // retrying in silence, which looks like a hang.
    let options = ConnectOptions { block_async_connect: true, strategy: ConnectStrategy::Fallback, ..Default::default() };
    client.connect(Some(options)).await.map_err(|e| PayError(format!("Could not reach the node at {}: {e}", settings.wrpc_url)))?;
    wallet.clone().start().await.map_err(|e| problem("start", e))?;
    Ok(wallet)
}

/// Make the wallet, one account in it, and say where rewards should be paid.
pub(crate) async fn init_wallet(settings: &WalletSettings) -> Result<WalletCreated, PayError> {
    let secret = Secret::from(settings.secret.expose().to_owned());
    let wallet = connect_wallet(settings).await?;

    let args = WalletCreateArgs {
        title: Some("Marigold pool".to_string()),
        filename: Some(WALLET_FILENAME.to_string()),
        encryption_kind: EncryptionKind::XChaCha20Poly1305,
        user_hint: None,
        overwrite_wallet_storage: false,
    };
    wallet.clone().wallet_create(secret.clone(), args).await.map_err(|e| {
        PayError(format!(
            "The pool wallet was not made. If one already exists in {}, it has been left alone. ({e})",
            settings.data_dir.display()
        ))
    })?;

    let mnemonic = Mnemonic::random(WordCount::Words12, Language::default()).map_err(|e| problem("make its recovery words", e))?;
    let key_id = wallet
        .clone()
        .prv_key_data_create(
            secret.clone(),
            PrvKeyDataCreateArgs::new(None, None, Secret::from(mnemonic.phrase()), PrvKeyDataVariantKind::Mnemonic),
        )
        .await
        .map_err(|e| problem("store its key", e))?;
    let descriptor = wallet
        .clone()
        .accounts_create(secret, AccountCreateArgs::new_bip32(key_id, None, None, None))
        .await
        .map_err(|e| problem("make its account", e))?;
    wallet.clone().accounts_activate(Some(vec![descriptor.account_id])).await.map_err(|e| problem("start its account", e))?;
    let account =
        wallet.active_accounts().get(&descriptor.account_id).ok_or_else(|| problem("find its account", "it is not active"))?;
    let address = account.receive_address().map_err(|e| problem("give its address", e))?;
    Ok(WalletCreated { address: address.to_string(), recovery_phrase: mnemonic.phrase_string() })
}

/// Open the wallet and return something that can pay with it.
pub(crate) async fn open_payer(settings: &WalletSettings) -> Result<SharedPayer, PayError> {
    let secret = Secret::from(settings.secret.expose().to_owned());
    let wallet = connect_wallet(settings).await?;
    let descriptors = wallet
        .clone()
        .wallet_open(secret.clone(), Some(WALLET_FILENAME.to_string()), true, false)
        .await
        .map_err(|e| {
            PayError(format!(
                "The pool wallet would not open. Check the password and that `init` was run in {}. ({e})",
                settings.data_dir.display()
            ))
        })?
        .ok_or_else(|| PayError("The pool wallet has no accounts. Run `init` first.".into()))?;
    let account_id =
        descriptors.first().ok_or_else(|| PayError("The pool wallet has no accounts. Run `init` first.".into()))?.account_id;
    wallet.clone().accounts_activate(Some(vec![account_id])).await.map_err(|e| problem("start its account", e))?;
    let account = wallet.active_accounts().get(&account_id).ok_or_else(|| problem("find its account", "it is not active"))?;

    // The balance is learned in the background once the account is active.
    let mut waited = Duration::ZERO;
    while account.balance().is_none() && waited < BALANCE_WAIT {
        workflow_core::task::sleep(Duration::from_millis(500)).await;
        waited += Duration::from_millis(500);
    }
    Ok(Arc::new(MarigoldPayer { wallet, account, secret }))
}

struct MarigoldPayer {
    wallet: Arc<Wallet>,
    account: Arc<dyn Account>,
    secret: Secret,
}

fn address(text: &str) -> Result<Address, PayError> {
    Address::try_from(text).map_err(|e| PayError(format!("{text:?} is not an address: {e}")))
}

#[async_trait::async_trait]
impl Payer for MarigoldPayer {
    async fn pay(&self, outputs: &[PayOutput], guard: &dyn SubmitGuard) -> Result<String, PayError> {
        let mut payments = Vec::with_capacity(outputs.len());
        for output in outputs {
            payments.push(PaymentOutput::new(address(&output.address)?, output.amount));
        }
        let destination = PaymentDestination::PaymentOutputs(PaymentOutputs { outputs: payments });

        // The change address the generator will use, which is where the change of
        // the payment transaction will be, so where to look for it later.
        let change = self.account.change_address().map_err(|e| problem("choose a change address", e))?.to_string();

        let keydata = self.account.prv_key_data(self.secret.clone()).await.map_err(|e| problem("unlock its key", e))?;
        let signer = Arc::new(Signer::new(self.account.clone(), keydata, None));
        // The pool pays the network fee on top of what miners are owed, so
        // every miner receives exactly their balance.
        let settings = GeneratorSettings::try_new_with_account(self.account.clone(), destination, None, Fees::SenderPays(0), None)
            .map_err(|e| problem("plan the payment", e))?;
        let abortable = Abortable::default();
        let generator = Generator::try_new(settings, Some(signer), Some(&abortable)).map_err(|e| problem("plan the payment", e))?;

        let rpc = self.wallet.rpc_api();
        let mut stream = generator.stream();
        let mut payment_txid = None;
        while let Some(transaction) = stream.try_next().await.map_err(|e| problem("build the payment", e))? {
            transaction.try_sign().map_err(|e| problem("sign the payment", e))?;
            if transaction.is_final() {
                // Everything before this one only gathered the pool's own coins
                // together. This is the one that pays miners: write its id down
                // first, and do not send it if that cannot be done.
                let txid = transaction.id().to_string();
                guard.before_broadcast(&txid, &change).await?;
                payment_txid = Some(txid);
            }
            transaction.try_submit(&rpc).await.map_err(|e| problem("send the payment", e))?;
        }
        payment_txid.ok_or_else(|| PayError("The wallet planned no payment transaction.".into()))
    }

    async fn sighting(&self, txid: &str, watch: &[String]) -> Result<TxSighting, PayError> {
        let rpc = self.wallet.rpc_api();
        let info = rpc.get_server_info().await.map_err(|e| problem("ask the node about its state", e))?;
        if !info.is_synced {
            return Err(PayError(
                "The node is still catching up with the network, so it cannot say yet whether a payment went through.".into(),
            ));
        }
        let id = Hash::from_str(txid).map_err(|e| PayError(format!("{txid:?} is not a transaction id: {e}")))?;
        if rpc.get_mempool_entry(id, false, false).await.is_ok() {
            return Ok(TxSighting::InMempool);
        }
        let mut addresses = Vec::with_capacity(watch.len());
        for text in watch {
            addresses.push(address(text)?);
        }
        let entries = rpc.get_utxos_by_addresses(addresses).await.map_err(|e| problem("look up outputs", e))?;
        if entries.iter().any(|entry| entry.outpoint.transaction_id == id) { Ok(TxSighting::InChain) } else { Ok(TxSighting::Unknown) }
    }

    async fn spendable(&self) -> Result<u64, PayError> {
        match self.account.balance() {
            Some(balance) => Ok(balance.mature),
            None => Err(PayError("The pool wallet has not finished loading its balance yet.".into())),
        }
    }
}
