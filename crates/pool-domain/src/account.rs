use crate::MinerAddress;

/// The key under which the pool's own fee share is kept. It is not a valid
/// address on any chain, so it can never collide with a miner.
pub const POOL_ACCOUNT: &str = "pool";

/// Who a sum of money belongs to inside the pool's books: the pool itself (its
/// fee) or a miner.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Account {
    Pool,
    Miner(MinerAddress),
}

impl Account {
    /// The text this account is stored under.
    pub fn key(&self) -> &str {
        match self {
            Account::Pool => POOL_ACCOUNT,
            Account::Miner(address) => address.as_str(),
        }
    }

    /// The account stored under `key`, for values read back from the database.
    pub fn from_key(key: String) -> Self {
        if key == POOL_ACCOUNT { Account::Pool } else { Account::Miner(MinerAddress::from_trusted(key)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        let pool = Account::from_key(Account::Pool.key().to_owned());
        assert_eq!(pool, Account::Pool);
        let miner = Account::Miner(MinerAddress::from_trusted("fake:abc".into()));
        assert_eq!(Account::from_key(miner.key().to_owned()), miner);
    }

    #[test]
    fn the_pool_key_is_not_an_address() {
        assert_eq!(POOL_ACCOUNT, "pool");
        assert!(!POOL_ACCOUNT.contains(':'));
    }
}
