INSERT INTO last_state (
    currency_address, 
    staker_address, 
    last_round, 
    last_work
) VALUES ($1, $2, $3, $4)
ON CONFLICT (currency_address, staker_address) 
DO UPDATE 
SET latest_round = EXCLUDED.latest_round, latest_work = EXCLUDED.latest_work
WHERE latest_state.currency_address = EXCLUDED.currency_address 
    AND latest_state.staker_address = EXCLUDED.staker_address