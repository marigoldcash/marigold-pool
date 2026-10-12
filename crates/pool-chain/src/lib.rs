//! The one place a chain is named (PLAN.md, Architecture and ground rule 8).
//!
//! Exactly one backend feature is compiled in: `chain-marigold` (the default) or
//! `chain-kaspa`. Everything above this crate asks the `Chain` trait for the
//! ticker, the unit, the address prefix, the ports and the maturity, and never
//! hard-codes them.

#[cfg(all(feature = "chain-marigold", feature = "chain-kaspa"))]
compile_error!("pick one chain: build with either `chain-marigold` or `chain-kaspa`, not both");

#[cfg(not(any(feature = "chain-marigold", feature = "chain-kaspa")))]
compile_error!("pick one chain: build with `chain-marigold` (the default) or `chain-kaspa`");

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
pub trait Chain {
    /// "MAGLD" or "KAS".
    fn ticker() -> &'static str;
    /// The smallest unit: "petal" or "sompi". Only ever shown to an operator.
    fn unit_name() -> &'static str;
    /// The address prefix for a network id such as "testnet-10", or `None` if
    /// this chain has no such network.
    fn address_prefix(network: &str) -> Option<&'static str>;
    /// The node's gRPC port on that network.
    fn default_grpc_port(network: &str) -> Option<u16>;
    /// The node's wRPC (borsh) port on that network.
    fn default_wrpc_port(network: &str) -> Option<u16>;
    /// How many DAA-score steps a coinbase output waits before it can be spent.
    fn coinbase_maturity_daa() -> u64;
    /// Whether `text` is an address on `network`, as a sentence a person can
    /// read when it is not.
    fn validate_address(network: &str, text: &str) -> Result<(), String>;
}

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
