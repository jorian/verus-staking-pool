-- Add migration script here
ALTER TABLE subscriptions RENAME COLUMN botaddress TO pool_address;