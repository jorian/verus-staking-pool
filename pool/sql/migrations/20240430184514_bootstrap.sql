
CREATE TYPE staker_status AS ENUM (
    'ACTIVE',
    'COOLING_DOWN',
    'INACTIVE'
);

CREATE TABLE stakers (
    currency_address TEXT NOT NULL,
    identity_address TEXT NOT NULL,
    identity_name TEXT NOT NULL,
    status staker_status NOT NULL,
    min_payout BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (currency_address, identity_address)
);

CREATE TABLE work (
    currency_address TEXT NOT NULL,
    round bigint NOT NULL,
    staker_address TEXT NOT NULL,
    shares DECIMAL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (currency_address, round, staker_address)
);

CREATE TYPE stake_status AS ENUM (
    'MATURING',
    'MATURED',
    'STALE',
    'STOLEN'
);

CREATE TABLE stakes (
    currency_address TEXT NOT NULL,
    block_hash TEXT NOT NULL,
    block_height BIGINT NOT NULL,
    amount BIGINT NOT NULL,
    found_by TEXT NOT NULL,
    source_txid TEXT NOT NULL,
    source_vout_num INT NOT NULL,
    source_amount BIGINT NOT NULL,
    status stake_status NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY(currency_address, block_hash)
);

CREATE OR REPLACE FUNCTION trigger_set_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;

$$ language 'plpgsql';

CREATE TRIGGER set_updated_timestamp BEFORE UPDATE ON stakers FOR EACH ROW EXECUTE PROCEDURE trigger_set_timestamp();
CREATE TRIGGER set_updated_timestamp BEFORE UPDATE ON stakes FOR EACH ROW EXECUTE PROCEDURE trigger_set_timestamp();
CREATE TRIGGER set_updated_timestamp BEFORE UPDATE ON work FOR EACH ROW EXECUTE PROCEDURE trigger_set_timestamp();
