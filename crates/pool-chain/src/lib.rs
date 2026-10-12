//! The one place a chain is named (PLAN.md, Architecture and ground rule 8).
//!
//! Exactly one backend feature is compiled into a binary: `chain-marigold` (the
//! default) or `chain-kaspa`. Everything above this crate asks the `Chain`
//! trait for the ticker, the unit, the address prefix, the ports and the
//! maturity, and never hard-codes them. The binary refuses to build with no
//! backend at all; this crate does not, so the crates that only need the trait
//! can be built and tested on their own.

#[cfg(all(feature = "chain-marigold", feature = "chain-kaspa"))]
compile_error!("pick one chain: build with either `chain-marigold` or `chain-kaspa`, not both");

/// Which backend this binary was built with, as the word an operator writes in
/// `[chain] name = "..."` in the config file.
#[cfg(feature = "chain-marigold")]
pub const BACKEND: &str = "marigold";

/// Which backend this binary was built with.
#[cfg(feature = "chain-kaspa")]
pub const BACKEND: &str = "kaspa";

/// What differs between chains. The three factories the plan lists (the bridge,
/// the payout wallet, the node view) are added by the steps that define the
/// types they return: the node view in M4, the wallet in M6, the bridge in M9.
///
/// A network is named by the id an operator writes in the config file, such as
/// "mainnet" or "testnet-10". Every question about a network returns `None`
/// (or an error sentence) for an id the chain does not have.
pub trait Chain {
    /// "MAGLD" or "KAS".
    fn ticker() -> &'static str;
    /// The smallest unit: "petal" or "sompi". Only ever shown to an operator.
    fn unit_name() -> &'static str;
    /// The address prefix on that network, such as "marigoldtest".
    fn address_prefix(network: &str) -> Option<&'static str>;
    /// The node's gRPC port on that network.
    fn default_grpc_port(network: &str) -> Option<u16>;
    /// The node's wRPC (borsh) port on that network.
    fn default_wrpc_port(network: &str) -> Option<u16>;
    /// How many DAA-score steps a coinbase output waits before it can be spent.
    fn coinbase_maturity_daa(network: &str) -> Option<u64>;
    /// Whether `text` is an address on `network`. When it is not, the error is a
    /// sentence a person can read and act on.
    fn validate_address(network: &str, text: &str) -> Result<(), String>;
}

mod events;
pub use events::{BlockReport, PoolEvents, ShareReport};

#[cfg(feature = "chain-marigold")]
mod marigold;
#[cfg(feature = "chain-marigold")]
pub use marigold::MarigoldChain;

/// The backend this binary was built with.
#[cfg(feature = "chain-marigold")]
pub type ActiveChain = MarigoldChain;

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(feature = "chain-marigold")]
    fn the_default_build_is_the_marigold_backend() {
        assert_eq!(super::BACKEND, "marigold");
    }

    #[test]
    #[cfg(feature = "chain-kaspa")]
    fn the_kaspa_build_is_the_kaspa_backend() {
        assert_eq!(super::BACKEND, "kaspa");
    }
}
