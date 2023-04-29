-- Add migration script here
ALTER TABLE payout_members ADD COLUMN payment_txid CHARACTER varying(64);