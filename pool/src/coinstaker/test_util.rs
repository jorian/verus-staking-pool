pub struct MockCoinStaker {
    pool: PgPool,
    chain_id: CurrencyId,
}

impl Staker for MockCoinStaker {
    async fn get_staking_supply(
        &self,
        _identity_addresses: Vec<IdentityAddress>,
    ) -> Result<StakingSupply> {
        Ok(StakingSupply {
            staker: 1.0,
            pool: 2.0,
            network: 3.0,
        })
    }

    async fn check_active_stakers(&self, verus_client: &VerusClient, block: &Block) -> Result<()> {
        for tx in &block.tx {
            for vout in &tx.vout {
                if let Some(identity_primary) = &vout.script_pubkey.identityprimary {
                    self.check_staker_status(
                        &verus_client,
                        &(&identity_primary.identityaddress).into(),
                    )
                    .await?;
                }
            }
        }
        // requirements
        // - get_identity() for every identityprimary in transaction vouts
        // -
        Ok(())
    }

    async fn check_staker_status(
        &self,
        client: &VerusClient,
        identity_address: &IdentityAddress,
    ) -> Result<()> {
        let identity = Identity {
            fullyqualifiedname: "alice@".to_string(),
            identity: IdentityPrimary {
                version: 1,
                flags: 2,
                primaryaddresses: vec![
                    Address::from_str("RDVXn9BFJMwtXsCkxs6Ru6wDSVe8jH9Qy2").unwrap()
                ],
                minimumsignatures: 1,
                name: "alice".to_string(),
                identityaddress: Address::from_str("i5f5njYtso65186mo5WHMkRme9YG6hrZE2").unwrap(),
                parent: Address::from_str("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq").unwrap(),
                systemid: Address::from_str("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq").unwrap(),
                contentmap: Value::Null,
                revocationauthority: Address::from_str("i5f5njYtso65186mo5WHMkRme9YG6hrZE2")
                    .unwrap(),
                recoveryauthority: Address::from_str("i5f5njYtso65186mo5WHMkRme9YG6hrZE2").unwrap(),
                privateaddress: None,
                timelock: 0,
                txout: None,
            },
            status: "active".to_string(),
            canspendfor: false,
            cansignfor: false,
            blockheight: 1125,
            txid: Txid::from_str(
                "6ea5e252e4c05d9ff892967fde037c6f9393218e2aa5675c3400e938f7d5bbef",
            )
            .unwrap(),
            vout: 0,
        };

        if let Some(staker) = database::get_staker(
            &self.pool,
            &self.chain_id,
            &identity.identity.identityaddress,
        )
        .await?
        {
            match staker.status {
                StakerStatus::Active => {
                    // if staker is not eligible anymore, make inactive
                    // self.webhooks.send(deactivated)
                }
                // any previously active stakers that update their identity, become
                // inactive.
                StakerStatus::Inactive => {
                    // if staker became eligible, activate
                    // self.webhooks.send(activated)
                }
            }
        }

        Ok(())
    }
}
