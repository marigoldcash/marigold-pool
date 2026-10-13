//! `marigold-pool check`: read the settings file and say what the pool would do,
//! or what is wrong with it. Starts nothing and touches nothing.

use crate::config::PoolConfig;
use clap::Args;
use pool_chain::Chain;
use std::path::PathBuf;

#[derive(Args)]
pub struct CheckArgs {
    /// The pool's settings file.
    #[arg(long, default_value = "/etc/marigold/pool.toml")]
    pub config: PathBuf,
}

pub fn run<C: Chain>(args: CheckArgs) -> Result<String, String> {
    let config = PoolConfig::load::<C>(&args.config).map_err(|e| e.to_string())?;
    Ok(format!("{}\n\nThe settings are fine.", config.summary::<C>()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pool_chain::MarigoldChain;

    fn write(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn a_good_file_is_summarised_in_words() {
        let shipped = include_str!("../../../deploy/pool.toml").replacen(
            "address = \"\"",
            "address = \"marigoldtest:qqfc69eulu3v8qamcasxqcx3wxvsfzjkv73fau4exk5kem70nqjsqgm3wm9g8\"",
            1,
        );
        let (_dir, path) = write(&shipped);
        let text = run::<MarigoldChain>(CheckArgs { config: path }).unwrap();
        assert!(text.contains("Network: testnet-10") && text.ends_with("The settings are fine."), "{text}");
    }

    #[test]
    fn the_shipped_file_as_it_stands_points_at_init() {
        let (_dir, path) = write(include_str!("../../../deploy/pool.toml"));
        let error = run::<MarigoldChain>(CheckArgs { config: path.clone() }).unwrap_err();
        assert!(error.starts_with(&format!("The settings file {} has problems:", path.display())), "{error}");
        assert!(error.contains("[pool] address is empty. Run `marigold-pool init`"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_there_is_named() {
        let error = run::<MarigoldChain>(CheckArgs { config: PathBuf::from("/nonexistent/pool.toml") }).unwrap_err();
        assert!(error.starts_with("The settings file /nonexistent/pool.toml could not be read:"), "{error}");
    }
}
