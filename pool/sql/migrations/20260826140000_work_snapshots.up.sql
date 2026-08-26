CREATE TABLE work_snapshots (
    currency_address TEXT NOT NULL,
    height BIGINT NOT NULL,
    shares DECIMAL NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (currency_address, height)
);

CREATE INDEX work_snapshots_currency_height
    ON work_snapshots (currency_address, height);
