-- Add migration script here
ALTER TABLE stakes ADD COLUMN pos_source_amount BIGINT NOT NULL;