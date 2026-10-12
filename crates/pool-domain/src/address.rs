use pool_chain::Chain;
use serde::Serialize;
use std::fmt;

/// Why a miner's address was refused, as a sentence that can be shown to them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AddressError(pub String);

/// The address a miner logged in with and is paid to. It can only be made by
/// `parse`, which asks the chain whether the text is an address on the pool's
/// network, so a value of this type has already been checked.
///
/// It serializes as the plain string and deliberately does not deserialize:
/// reading one back from the database goes through `from_trusted`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MinerAddress(String);

impl MinerAddress {
    /// Check `text` (surrounding spaces are ignored) against `network` through
    /// the chain `C`.
    pub fn parse<C: Chain>(network: &str, text: &str) -> Result<Self, AddressError> {
        let text = text.trim();
        C::validate_address(network, text).map_err(AddressError)?;
        Ok(MinerAddress(text.to_owned()))
    }

    /// Wrap text that was already checked when it was first stored. Only for
    /// reading values back out of the pool's own database.
    pub fn from_trusted(text: String) -> Self {
        MinerAddress(text)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MinerAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_chain::FakeChain;

    #[test]
    fn a_good_address_is_kept_as_written() {
        let address = MinerAddress::parse::<FakeChain>("fakenet", "fake:abc").unwrap();
        assert_eq!(address.as_str(), "fake:abc");
        assert_eq!(address.to_string(), "fake:abc");
    }

    #[test]
    fn spaces_around_an_address_are_ignored() {
        let address = MinerAddress::parse::<FakeChain>("fakenet", "  fake:abc \n").unwrap();
        assert_eq!(address.as_str(), "fake:abc");
    }

    #[test]
    fn a_bad_address_is_refused_with_the_chains_own_sentence() {
        let error = MinerAddress::parse::<FakeChain>("fakenet", "nonsense").unwrap_err();
        assert_eq!(error.to_string(), "That does not look like an address. It starts with fake:.");
        let error = MinerAddress::parse::<FakeChain>("othernet", "fake:abc").unwrap_err();
        assert_eq!(error.to_string(), "This network does not exist.");
    }

    #[test]
    fn it_serializes_as_a_plain_string() {
        let address = MinerAddress::from_trusted("fake:abc".into());
        assert_eq!(serde_json::to_string(&address).unwrap(), "\"fake:abc\"");
    }
}
