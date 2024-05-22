INSERT INTO stakes(
    currency_address, 
    block_hash, 
    block_height, 
    amount, 
    found_by, 
    source_txid, 
    source_vout_num, 
    source_amount, 
    status
)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
ON CONFLICT (currency_address, block_hash) DO 
UPDATE SET 
    status = $9;
