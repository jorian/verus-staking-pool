-- Add migration script here
ALTER TABLE public.payouts RENAME COLUMN bot_fee_amount TO pool_fee_amount;
