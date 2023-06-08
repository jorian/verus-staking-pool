-- Add migration script here
ALTER TABLE payouts ADD COLUMN blockheight BIGINT NOT NULL;
ALTER TABLE payout_members ADD COLUMN blockheight BIGINT NOT NULL;