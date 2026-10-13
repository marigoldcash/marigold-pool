#[cfg(not(any(feature = "chain-marigold", feature = "chain-kaspa")))]
compile_error!("pick one chain: build with `chain-marigold` (the default) or `chain-kaspa`");

mod api_cmd;
mod check_cmd;
mod common;
mod config;
mod init_cmd;
mod miner_cmd;
mod payout_cmd;
mod rewards_cmd;
mod serve;

use clap::{CommandFactory, Parser, Subcommand};
use std::process::ExitCode;

/// Mining pool for Marigold.
#[derive(Parser)]
#[command(name = "marigold-pool", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List the block rewards that have reached an address, and which of them are ready to be split. Reads from the node; changes nothing.
    Rewards(rewards_cmd::RewardsArgs),
    /// Make the pool wallet and say which address block rewards should be paid to. The wallet password comes from the environment.
    Init(init_cmd::InitArgs),
    /// Pay what miners are owed. Use --now to do it straight away.
    Payout(payout_cmd::PayoutArgs),
    /// Serve the pool's pages and API from its database, read-only.
    Api(api_cmd::ApiArgs),
    /// Read the settings file and say what the pool would do, or what is wrong with it. Starts nothing.
    Check(check_cmd::CheckArgs),
    /// Look after a miner's settings. For the operator.
    Miner(miner_cmd::MinerArgs),
    /// Run the pool: stratum, shares, rewards, payouts and the pages, until asked to stop. The wallet password comes from the environment.
    Serve(serve::ServeArgs),
}

/// Messages from the pool go to standard error, which a service's journal collects.
/// Under systemd the journal adds the time itself.
fn start_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(false);
    if std::env::var_os("INVOCATION_ID").is_some() { builder.without_time().init() } else { builder.init() }
}

fn secret_from_environment() -> Option<String> {
    std::env::var(common::SECRET_VARIABLE).ok()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn finish(outcome: Result<String, String>) -> ExitCode {
    match outcome {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Rewards(args)) => finish(rewards_cmd::run::<pool_chain::ActiveChain>(args).await),
        Some(Command::Init(args)) => finish(init_cmd::run::<pool_chain::ActiveChain>(args, secret_from_environment()).await),
        Some(Command::Api(args)) => finish(api_cmd::run::<pool_chain::ActiveChain>(args).await),
        Some(Command::Miner(args)) => finish(miner_cmd::run::<pool_chain::ActiveChain>(args).await),
        Some(Command::Check(args)) => finish(check_cmd::run::<pool_chain::ActiveChain>(args)),
        Some(Command::Serve(args)) => {
            start_logging();
            finish(serve::serve_command::<pool_chain::ActiveChain>(args, secret_from_environment()).await)
        }
        Some(Command::Payout(args)) => {
            finish(payout_cmd::run::<pool_chain::ActiveChain>(args, secret_from_environment(), now_ms()).await)
        }
        None => {
            let _ = Cli::command().print_help();
            ExitCode::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn the_version_flag_prints_name_and_version() {
        let text = super::Cli::command().render_version();
        assert_eq!(text.trim(), format!("marigold-pool {}", env!("CARGO_PKG_VERSION")));
    }
}
