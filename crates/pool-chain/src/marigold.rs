//! The Marigold backend. Ports, prefixes and maturity come from the node's own
//! network definitions rather than being repeated here, so they cannot drift
//! from the node this pool is pinned to.

use crate::Chain;
use kaspa_addresses::{Address, Prefix};
use kaspa_consensus_core::config::params::Params;
use kaspa_consensus_core::network::{NetworkId, NetworkType};
use std::str::FromStr;

pub struct MarigoldChain;

/// The network types the pool supports: mainnet, testnet-10, and the two local
/// development networks. Any other id (an unknown word, `testnet-11`,
/// `mainnet-3`) is not one of them.
fn network_type(id: &str) -> Option<NetworkType> {
    let id = NetworkId::from_str(id).ok()?;
    match (id.network_type(), id.suffix()) {
        (NetworkType::Testnet, Some(10)) => Some(NetworkType::Testnet),
        (NetworkType::Testnet, _) => None,
        (other, None) => Some(other),
        (_, Some(_)) => None,
    }
}

fn describe(network: NetworkType) -> &'static str {
    match network {
        NetworkType::Mainnet => "the main network",
        NetworkType::Testnet => "the test network",
        NetworkType::Devnet => "the development network",
        NetworkType::Simnet => "the simulation network",
    }
}

fn prefix_str(prefix: Prefix) -> &'static str {
    match prefix {
        Prefix::Mainnet => "marigold",
        Prefix::Testnet => "marigoldtest",
        Prefix::Simnet => "marigoldsim",
        Prefix::Devnet => "marigolddev",
        #[allow(unreachable_patterns)]
        _ => "marigold",
    }
}

impl Chain for MarigoldChain {
    fn ticker() -> &'static str {
        "MAGLD"
    }

    fn unit_name() -> &'static str {
        "petal"
    }

    fn address_prefix(network: &str) -> Option<&'static str> {
        network_type(network).map(|n| prefix_str(Prefix::from(n)))
    }

    fn default_grpc_port(network: &str) -> Option<u16> {
        network_type(network).map(|n| n.default_rpc_port())
    }

    fn default_wrpc_port(network: &str) -> Option<u16> {
        network_type(network).map(|n| n.default_borsh_rpc_port())
    }

    fn coinbase_maturity_daa(network: &str) -> Option<u64> {
        network_type(network).map(|n| Params::from(n).coinbase_maturity())
    }

    fn validate_address(network: &str, text: &str) -> Result<(), String> {
        let Some(kind) = network_type(network) else {
            return Err(format!(
                "The pool is set to a network called \"{network}\", which Marigold does not have. Use mainnet or testnet-10 in the pool's settings."
            ));
        };
        let wanted = Prefix::from(kind);
        let Ok(address) = Address::try_from(text) else {
            return Err(format!(
                "That does not look like a Marigold address. It starts with {}: on {}.",
                prefix_str(wanted),
                describe(kind)
            ));
        };
        if address.prefix != wanted {
            return Err(format!(
                "That address is for a different network. On {}, addresses start with {}:.",
                describe(kind),
                prefix_str(wanted)
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_addresses::Version;

    fn address(prefix: Prefix) -> String {
        Address::new(prefix, Version::PubKey, &[7u8; 32]).to_string()
    }

    #[test]
    fn the_names_are_marigolds() {
        assert_eq!(MarigoldChain::ticker(), "MAGLD");
        assert_eq!(MarigoldChain::unit_name(), "petal");
    }

    #[test]
    fn prefixes_follow_the_network() {
        assert_eq!(MarigoldChain::address_prefix("mainnet"), Some("marigold"));
        assert_eq!(MarigoldChain::address_prefix("testnet-10"), Some("marigoldtest"));
        assert_eq!(MarigoldChain::address_prefix("testnet"), Some("marigoldtest"), "a bare testnet means testnet-10");
    }

    #[test]
    fn ports_are_the_nodes_defaults() {
        assert_eq!(MarigoldChain::default_grpc_port("mainnet"), Some(26110));
        assert_eq!(MarigoldChain::default_grpc_port("testnet-10"), Some(26210));
        assert_eq!(MarigoldChain::default_wrpc_port("mainnet"), Some(27110));
        assert_eq!(MarigoldChain::default_wrpc_port("testnet-10"), Some(27210));
    }

    #[test]
    fn a_coinbase_waits_a_thousand_steps() {
        assert_eq!(MarigoldChain::coinbase_maturity_daa("mainnet"), Some(1000));
        assert_eq!(MarigoldChain::coinbase_maturity_daa("testnet-10"), Some(1000));
    }

    #[test]
    fn networks_the_chain_does_not_have_are_refused() {
        for bad in ["", "kaspa", "testnet-11", "mainnet-3", "testnet-x", "mainnet-testnet-10"] {
            assert_eq!(MarigoldChain::address_prefix(bad), None, "{bad:?}");
            assert_eq!(MarigoldChain::default_grpc_port(bad), None, "{bad:?}");
            assert_eq!(MarigoldChain::coinbase_maturity_daa(bad), None, "{bad:?}");
        }
        let error = MarigoldChain::validate_address("testnet-11", &address(Prefix::Testnet)).unwrap_err();
        assert!(error.contains("testnet-11") && error.contains("testnet-10"), "{error}");
    }

    #[test]
    fn an_address_on_the_right_network_passes() {
        assert_eq!(MarigoldChain::validate_address("testnet-10", &address(Prefix::Testnet)), Ok(()));
        assert_eq!(MarigoldChain::validate_address("mainnet", &address(Prefix::Mainnet)), Ok(()));
    }

    #[test]
    fn an_address_on_the_wrong_network_says_so_in_words() {
        let error = MarigoldChain::validate_address("testnet-10", &address(Prefix::Mainnet)).unwrap_err();
        assert_eq!(error, "That address is for a different network. On the test network, addresses start with marigoldtest:.");
    }

    #[test]
    fn something_that_is_not_an_address_says_what_one_looks_like() {
        for junk in ["", "hello", "marigoldtest:nonsense", "kaspa:qqqqqqqq"] {
            let error = MarigoldChain::validate_address("testnet-10", junk).unwrap_err();
            assert_eq!(
                error, "That does not look like a Marigold address. It starts with marigoldtest: on the test network.",
                "{junk:?}"
            );
        }
    }
}
