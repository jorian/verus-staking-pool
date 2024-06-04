pub struct Worker {
    identity_address: Address,
    shares: Decimal,
    // fee is in basis points: 5% should be entered as 0.05
    fee: Decimal,
}
