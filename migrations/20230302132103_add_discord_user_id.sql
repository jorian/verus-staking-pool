-- Add migration script here
ALTER TABLE public.subscriptions
ADD COLUMN discord_user_id text NOT NULL;
