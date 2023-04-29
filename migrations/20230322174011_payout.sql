-- Add migration script here
ALTER TABLE public.payouts DROP CONSTRAINT payouts_pkey;
ALTER TABLE public.payouts DROP COLUMN txid;
ALTER TABLE public.payouts DROP COLUMN minedby;
ALTER TABLE public.payouts RENAME COLUMN botfee TO bot_fee_amount;
ALTER TABLE public.payouts ADD COLUMN amount_paid_to_subs bigint NOT NULL;
ALTER TABLE public.payouts ADD COLUMN n_subs bigint NOT NULL;
ALTER TABLE public.payouts ADD PRIMARY KEY (currencyid, blockhash);

ALTER TABLE public.pending_txns RENAME TO stakes;
ALTER TABLE public.stakes DROP CONSTRAINT pending_txns_pkey;
ALTER TABLE public.stakes DROP COLUMN txid;
ALTER TABLE public.stakes DROP COLUMN round;
ALTER TYPE outcome RENAME TO result;
ALTER TABLE public.stakes RENAME COLUMN outcome TO result;
ALTER TABLE public.stakes ADD COLUMN mined_by character varying(34) NOT NULL;
ALTER TABLE public.stakes ADD COLUMN amount bigint NOT NULL;
ALTER TABLE public.stakes ADD COLUMN blockheight bigint NOT NULL;
ALTER TABLE public.stakes ADD COLUMN blockhash character varying(64) NOT NULL;
ALTER TABLE public.stakes ADD PRIMARY KEY (currencyid, blockhash);


CREATE TABLE public.payout_members(
    currencyid character varying(34) NOT NULL,
    blockheight bigint NOT NULL,
    identityaddress character varying(34) NOT NULL,
    shares numeric NOT NULL,
    reward bigint NOT NULL,
    fee bigint NOT NULL, 

    PRIMARY KEY (currencyid, blockheight, identityaddress)
)

