INSERT INTO work(
    currency_address, 
    round, 
    staker_address, 
    shares
) VALUES ($1, $2, $3, $4)
ON CONFLICT ON CONSTRAINT work_pkey 
DO UPDATE
SET shares = work.shares + EXCLUDED.shares
WHERE work.currency_address = EXCLUDED.currency_address 
    AND work.round = EXCLUDED.round 
    AND work.staker_address = EXCLUDED.staker_address
