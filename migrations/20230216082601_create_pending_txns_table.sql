CREATE TYPE status as ENUM ('pending', 'processed');
CREATE TYPE outcome AS ENUM ('mature', 'stale');

CREATE TABLE public.pending_txns(
    currencyidhex character varying(40) NOT NULL,
    txid character varying(64) NOT NULL,
    round text NOT NULL,
    status text NOT NULL,
    outcome text,

    PRIMARY KEY (currencyidhex, txid)
)