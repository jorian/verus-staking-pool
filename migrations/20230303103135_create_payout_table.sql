-- Add migration script here
-- Add migration script here
CREATE TABLE public.payouts(
    currencyidhex character varying(40) NOT NULL, -- which chain the payout is for
    txid character varying(64) NOT NULL, -- txid of payout
    amount bigint NOT NULL, -- amount that was paid out in sats
    totalwork NUMERIC NOT NULL, -- total shares in this round
    botfee bigint NOT NULL, -- how much the bot got paid in sats
    -- nparticipants -- how many stakers participated in this round // unnecessary because we can do a transactions tally per round
    blockhash character varying(64) NOT NULL, -- hash of block that was staked
    minedby character varying(52) NOT NULL, -- the identityaddress that staked the block connected to this payout

    PRIMARY KEY (currencyidhex, txid)
);

CREATE INDEX minedby_payouts_index 
ON payouts(minedby)