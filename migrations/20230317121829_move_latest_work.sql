-- Add migration script here
CREATE TABLE public.latest_state(
    currencyidhex character varying(40) NOT NULL,
    address character varying(52) NOT NULL,
    latest_round bigint NOT NULL,
    latest_work numeric NOT NULL
);

ALTER TABLE public.work DROP COLUMN latest_round;
ALTER TABLE public.work DROP COLUMN latest_work;

