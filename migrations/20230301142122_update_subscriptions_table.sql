-- Add migration script here
ALTER TABLE public.subscriptions
ALTER COLUMN identityname SET NOT NULL;
