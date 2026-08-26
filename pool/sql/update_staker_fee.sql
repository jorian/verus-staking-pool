UPDATE stakers
SET fee = $3
WHERE currency_address = $1
    AND identity_address = $2
RETURNING
    currency_address,
    identity_address,
    identity_name,
    min_payout,
    status AS "status: _",
    fee;
