-- Add migration script here
ALTER TABLE public.payouts RENAME currencyidhex TO currencyid;
ALTER TABLE public.payouts ALTER COLUMN currencyid TYPE CHARACTER VARYING(34);

ALTER TABLE public.pending_txns RENAME currencyidhex TO currencyid;
ALTER TABLE public.pending_txns ALTER COLUMN currencyid TYPE CHARACTER VARYING(34);

ALTER TABLE public.subscriptions RENAME currencyidhex TO currencyid;
ALTER TABLE public.subscriptions ALTER COLUMN currencyid TYPE CHARACTER VARYING(34);

ALTER TABLE public.transactions RENAME currencyidhex TO currencyid;
ALTER TABLE public.transactions ALTER COLUMN currencyid TYPE CHARACTER VARYING(34);

ALTER TABLE public.work RENAME currencyidhex TO currencyid;
ALTER TABLE public.work ALTER COLUMN currencyid TYPE CHARACTER VARYING(34);