-- Add migration script here
ALTER TABLE public.subscriptions ADD COLUMN fee bigint NOT NULL;