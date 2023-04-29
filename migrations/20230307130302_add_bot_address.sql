-- Add migration script here
ALTER TABLE subscriptions ADD COLUMN botaddress character varying(34) NOT NULL;