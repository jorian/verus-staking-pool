-- Add migration script here
ALTER TABLE subscriptions DROP COLUMN discord_user_id;
ALTER TABLE subscriptions ALTER COLUMN status SET NOT NULL;
ALTER TABLE subscriptions ALTER COLUMN min_payout DROP DEFAULT;