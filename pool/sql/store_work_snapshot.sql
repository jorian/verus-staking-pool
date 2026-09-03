INSERT INTO work_snapshots (
    currency_address,
    height,
    shares
) VALUES (
    $1, $2, $3
)
ON CONFLICT (currency_address, height) DO
UPDATE SET shares = EXCLUDED.shares;
