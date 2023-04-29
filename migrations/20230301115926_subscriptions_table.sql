-- Add migration script here
CREATE TABLE public.subscriptions(
    identityaddress character varying(52) NOT NULL,
    identityname text,
    status text,

    PRIMARY KEY (identityaddress)
)