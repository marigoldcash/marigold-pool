-- A miner's own settings, protected by a password they choose the first time
-- they enter their address on the pool's page. Only the hash of the password is
-- kept. A miner with no row here has set nothing and gets the pool's defaults.
CREATE TABLE miner_accounts (
    address         TEXT    PRIMARY KEY NOT NULL,
    password_hash   TEXT    NOT NULL,
    payout_level    INTEGER,                      -- NULL: the pool's own minimum
    failed_attempts INTEGER NOT NULL DEFAULT 0,   -- wrong passwords since the last right one
    locked_until_ms INTEGER NOT NULL DEFAULT 0,   -- no password is tried before this time
    created_ts_ms   INTEGER NOT NULL,
    updated_ts_ms   INTEGER NOT NULL
);
