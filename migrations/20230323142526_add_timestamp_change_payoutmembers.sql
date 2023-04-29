ALTER TABLE public.payout_members ADD COLUMN created_at TIMESTAMPTZ NOT NULL DEFAULT NOW();
ALTER TABLE public.payout_members ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

CREATE TRIGGER set_updated_timestamp BEFORE UPDATE ON public.payout_members FOR EACH ROW EXECUTE PROCEDURE trigger_set_timestamp();

ALTER TABLE public.payout_members ALTER COLUMN blockheight TYPE character varying(64);
ALTER TABLE public.payout_members RENAME COLUMN blockheight TO blockhash;