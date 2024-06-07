INSERT INTO payout_members (
    currency_address, 
    identity_address, 
    block_hash, 
    block_height, 
    shares, 
    reward, 
    fee,
    txid
) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
ON CONFLICT (currency_address, identity_address, block_hash)
DO UPDATE
SET txid = $8;
