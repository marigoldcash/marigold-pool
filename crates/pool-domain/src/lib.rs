//! Types and pure logic shared by every other pool crate. Nothing here names a
//! chain: PLAN.md ground rule 8. Whatever differs between chains arrives through
//! the `pool_chain::Chain` trait.

mod account;
mod address;
mod allocation;
mod amount;
mod weight;
mod worker;

pub use account::{Account, POOL_ACCOUNT};
pub use address::{AddressError, MinerAddress};
pub use allocation::{Allocation, MAX_FEE_BP, allocate};
pub use amount::{Amount, AmountDisplay, UNITS_PER_COIN};
pub use weight::ShareWeight;
pub use worker::{DEFAULT_WORKER, MAX_WORKER_NAME_CHARS, WorkerName};

/// A chain for the tests in this crate, so they do not depend on a backend.
#[cfg(test)]
pub(crate) mod test_chain {
    use pool_chain::Chain;

    pub struct FakeChain;

    impl Chain for FakeChain {
        fn ticker() -> &'static str {
            "TST"
        }
        fn unit_name() -> &'static str {
            "grain"
        }
        fn address_prefix(network: &str) -> Option<&'static str> {
            (network == "fakenet").then_some("fake")
        }
        fn default_grpc_port(network: &str) -> Option<u16> {
            (network == "fakenet").then_some(1)
        }
        fn default_wrpc_port(network: &str) -> Option<u16> {
            (network == "fakenet").then_some(2)
        }
        fn coinbase_maturity_daa(network: &str) -> Option<u64> {
            (network == "fakenet").then_some(1000)
        }
        async fn connect_node(_grpc_url: &str) -> Result<pool_chain::SharedNode, pool_chain::NodeError> {
            Err(pool_chain::NodeError("there is no node in the tests".into()))
        }
        fn validate_address(network: &str, text: &str) -> Result<(), String> {
            if network != "fakenet" {
                return Err("This network does not exist.".into());
            }
            match text.strip_prefix("fake:") {
                Some(rest) if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_lowercase()) => Ok(()),
                _ => Err("That does not look like an address. It starts with fake:.".into()),
            }
        }
    }
}
