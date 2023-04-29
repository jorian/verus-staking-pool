-- Add migration script here
ALTER TABLE stakes ADD COLUMN pos_source_txid CHARACTER varying(64) not null;
ALTER TABLE stakes ADD COLUMN pos_source_vout_num SMALLINT NOT NULL;
