-- Add migration script here
ALTER TABLE subscriptions
    ADD COLUMN currencyidhex character varying(40) NOT NULL,
    DROP CONSTRAINT subscriptions_pkey,
    ADD PRIMARY KEY(currencyidhex, identityaddress);