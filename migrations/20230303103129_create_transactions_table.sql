-- This table is for recording transactions for every staker in the pool
-- how much the staker got paid out
-- how many shares he participated in this round
CREATE TABLE public.transactions(
    currencyidhex character varying(40) NOT NULL,
    txid character varying(64) NOT NULL,
    identityaddress character varying(52) NOT NULL,
    amount bigint NOT NULL, -- amount paid to identityaddress in sats
    shares NUMERIC NOT NULL,

    PRIMARY KEY (currencyidhex, txid)
);

CREATE INDEX identityaddress_transactions_index 
ON transactions(identityaddress)