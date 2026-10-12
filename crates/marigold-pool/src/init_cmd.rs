//! `marigold-pool init`: make the pool wallet.

use crate::common::PoolArgs;
use clap::Args;
use pool_chain::Chain;

#[derive(Args)]
pub struct InitArgs {
    #[command(flatten)]
    pub pool: PoolArgs,
}

/// Make the wallet and return the text to print. The recovery words are in it,
/// once; nothing else keeps them.
pub async fn run<C: Chain>(args: InitArgs, secret: Option<String>) -> Result<String, String> {
    let settings = args.pool.wallet_settings::<C>(secret)?;
    std::fs::create_dir_all(&settings.data_dir)
        .map_err(|e| format!("Could not make the data directory {}: {e}", settings.data_dir.display()))?;
    let created = C::init_wallet(&settings).await.map_err(|e| e.to_string())?;
    Ok(format!(
        "The pool wallet was made in {dir}.\n\
         \n\
         Block rewards should be paid to this address:\n\
         \x20 {address}\n\
         \n\
         RECOVERY WORDS. Write them down now and keep them somewhere safe. They are shown this once, and the pool does not keep them:\n\
         \x20 {words}\n\
         \n\
         Put the address in the pool's settings as the pool address.",
        dir = settings.data_dir.join("wallet").display(),
        address = created.address,
        words = created.recovery_phrase,
    ))
}
