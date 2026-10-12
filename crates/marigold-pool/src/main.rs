#[cfg(not(any(feature = "chain-marigold", feature = "chain-kaspa")))]
compile_error!("pick one chain: build with `chain-marigold` (the default) or `chain-kaspa`");

mod rewards_cmd;

use clap::{CommandFactory, Parser, Subcommand};
use std::process::ExitCode;

/// Mining pool for Marigold. Most of it is not runnable yet; the first runnable version is M9.
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
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Rewards(args)) => match rewards_cmd::run::<pool_chain::ActiveChain>(args).await {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("{message}");
                ExitCode::FAILURE
            }
        },
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
