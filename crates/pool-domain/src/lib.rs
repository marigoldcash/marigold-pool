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

/// The chain for this crate's tests.
#[cfg(test)]
pub(crate) mod test_chain {
    pub use pool_chain::testing::FakeChain;
}
