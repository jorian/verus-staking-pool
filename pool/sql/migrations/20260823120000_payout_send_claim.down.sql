DROP INDEX IF EXISTS payout_members_in_flight;

ALTER TABLE payout_members
    DROP COLUMN IF EXISTS payment_opid,
    DROP COLUMN IF EXISTS payment_batch_id;
