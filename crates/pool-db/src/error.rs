/// What can go wrong talking to the pool's database. These are for the
/// operator's log; nothing here is shown to a miner.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("schema update failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("a number does not fit in the database ({0})")]
    OutOfRange(&'static str),
    #[error("the database holds something unexpected: {0}")]
    Corrupt(String),
    #[error("reward {0} is not in the database")]
    UnknownReward(String),
    #[error("reward {0} has already been split")]
    AlreadyAllocated(String),
    #[error("the split adds up to {got} but reward {outpoint} is {expected}")]
    AllocationMismatch { outpoint: String, expected: u64, got: u64 },
    #[error("the split names {0} twice")]
    DuplicateAccount(String),
    #[error("{address} is owed {have} but {wanted} was asked for")]
    InsufficientBalance { address: String, have: u64, wanted: u64 },
    #[error("a payout with key {0} already exists")]
    DuplicatePayoutKey(String),
    #[error("payout {0} is not in the database")]
    UnknownPayout(i64),
    #[error("payout {id} is {found}, which cannot go to {wanted}")]
    WrongPayoutState { id: i64, found: String, wanted: &'static str },
}
