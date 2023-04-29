-- Add migration script here
ALTER TABLE public.subscriptions ADD COLUMN min_payout bigint not null default 1000000000; -- 10.00000000