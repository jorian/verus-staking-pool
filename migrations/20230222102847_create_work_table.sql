-- Add migration script here
CREATE TABLE public.work(
    currencyidhex character varying(40) NOT NULL,
    round bigint NOT NULL,
    address character varying(52) NOT NULL,
    shares bigint,

    PRIMARY KEY (currencyidhex, round, address)
)