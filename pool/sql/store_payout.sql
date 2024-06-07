INSERT INTO payouts (
    currency_address, 
    block_hash, 
    block_height, 
    amount, 
    work, 
    fee, 
    amount_paid, 
    n_subs
) 
VALUES ($1, $2, $3, $4, $5, $6, $7, $8);
