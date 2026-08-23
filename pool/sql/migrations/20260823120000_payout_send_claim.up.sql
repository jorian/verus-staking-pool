ALTER TABLE payout_members
    ADD COLUMN payment_batch_id UUID,
    ADD COLUMN payment_opid TEXT;

CREATE INDEX payout_members_in_flight
    ON payout_members (currency_address)
    WHERE txid IS NULL AND payment_batch_id IS NOT NULL;
