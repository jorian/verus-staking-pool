INSERT INTO stakers (
    currency_address,
    identity_address,
    identity_name,
    status,
    min_payout, 
    fee
) VALUES (
    $1, $2, $3, $4, $5, $6
) 
ON CONFLICT (currency_address, identity_address) DO 
UPDATE SET 
    status = $4,
    min_payout = $5,
    fee = $6;
