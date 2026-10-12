-- Marigold pool: the first schema.
--
-- Every amount is a whole number of the chain's smallest unit. Every *_ms and
-- ts_ms column is milliseconds since the Unix epoch. A block's nonce is a u64
-- stored as the i64 with the same bits, because SQLite integers are signed.

CREATE TABLE miners (
    address    TEXT    PRIMARY KEY NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL
);

CREATE TABLE shares (
    id         INTEGER PRIMARY KEY,
    address    TEXT    NOT NULL,
    worker     TEXT    NOT NULL,
    weight     INTEGER NOT NULL,
    hash_value REAL    NOT NULL,
    daa_score  INTEGER NOT NULL,
    ts_ms      INTEGER NOT NULL
);
CREATE INDEX idx_shares_daa ON shares (daa_score);
CREATE INDEX idx_shares_ts  ON shares (ts_ms);

CREATE TABLE blocks (
    hash       TEXT    PRIMARY KEY NOT NULL,
    address    TEXT    NOT NULL,
    worker     TEXT    NOT NULL,
    blue_score INTEGER NOT NULL,
    daa_score  INTEGER NOT NULL,
    nonce      INTEGER NOT NULL,
    ts_ms      INTEGER NOT NULL,
    blue       INTEGER           -- NULL until the block's colour is known; 1 blue, 0 red
);
CREATE INDEX idx_blocks_ts ON blocks (ts_ms);

CREATE TABLE rewards (
    outpoint        TEXT    PRIMARY KEY NOT NULL,
    amount          INTEGER NOT NULL CHECK (amount > 0),
    block_daa_score INTEGER NOT NULL,
    seen_ts_ms      INTEGER NOT NULL,
    allocated_ts_ms INTEGER           -- NULL until the reward has been split
);

-- 'pool' is the key of the pool's own fee; it is not a valid address.
CREATE TABLE balances (
    address      TEXT    PRIMARY KEY NOT NULL,
    balance      INTEGER NOT NULL DEFAULT 0 CHECK (balance >= 0),
    total_earned INTEGER NOT NULL DEFAULT 0,
    total_paid   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE allocations (
    reward_outpoint TEXT    NOT NULL,
    address         TEXT    NOT NULL,
    amount          INTEGER NOT NULL,
    PRIMARY KEY (reward_outpoint, address)
);

CREATE TABLE payouts (
    id              INTEGER PRIMARY KEY,
    idempotency_key TEXT    NOT NULL UNIQUE,
    address         TEXT    NOT NULL,
    amount          INTEGER NOT NULL CHECK (amount > 0),
    status          TEXT    NOT NULL CHECK (status IN ('pending', 'sent', 'confirmed', 'failed')),
    txid            TEXT,
    note            TEXT,             -- why a payout failed, for the operator
    created_ts_ms   INTEGER NOT NULL,
    updated_ts_ms   INTEGER NOT NULL
);
CREATE INDEX idx_payouts_status  ON payouts (status);
CREATE INDEX idx_payouts_address ON payouts (address);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
