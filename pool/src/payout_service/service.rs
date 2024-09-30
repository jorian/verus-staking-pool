use std::{collections::HashMap, time::Duration};

use anyhow::{bail, Result};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemHandle};
use tracing::{debug, error, info, trace};
use vrsc_rpc::{
    bitcoin::Txid,
    client::{Client, RpcApi, SendCurrencyOutput},
    json::vrsc::{Address, Amount},
};

use crate::{
    coinstaker::{ChainConfig, PayoutConfig as PayoutServiceConfig},
    database::{self},
};

use super::{payout::Payout, PayoutMember};

pub struct Service {
    database: PgPool,
    config: PayoutServiceConfig,
    chain_id: Address,
    pool_address: Address,
    chain_config: ChainConfig,
}

impl Service {
    pub fn new(
        config: PayoutServiceConfig,
        database: PgPool,
        chain_id: Address,
        pool_address: Address,
        chain_config: ChainConfig,
    ) -> Self {
        Self {
            database,
            config,
            chain_id,
            pool_address,
            chain_config,
        }
    }

    async fn new_payout(&self) -> Result<()> {
        // TODO can be null at first start
        let last_sync_id = database::get_payout_sync_id(&self.database, &self.chain_id)
            .await?
            .unwrap_or(0);

        let stakes = database::get_stakes_by_status(
            &self.database,
            &self.chain_id,
            crate::coinstaker::constants::StakeStatus::Matured,
            Some(last_sync_id),
        )
        .await?;

        for stake in stakes {
            let workers =
                database::get_workers_by_round(&self.database, &self.chain_id, stake.block_height)
                    .await?;

            let mut tx = self.database.begin().await?;

            let payout = Payout::new(&stake, workers, Decimal::ZERO)?;

            database::store_payout(&mut tx, &payout).await?;

            for member in payout.members {
                database::store_payout_member(&mut tx, &member).await?;
            }

            database::update_last_payout_height(&mut tx, &self.chain_id, stake.block_height)
                .await?;

            tx.commit().await?;
        }

        Ok(())
    }

    async fn send_unsent_payouts(&self) -> Result<()> {
        let mut tx = self.database.begin().await?;

        let unpaid_payout_members =
            database::get_unpaid_payout_members(&mut tx, &self.chain_id).await?;

        if unpaid_payout_members.is_empty() {
            return Ok(());
        }

        let outputs = prepare_payment(&unpaid_payout_members)?;

        let client: vrsc_rpc::client::Client = (&self.chain_config).try_into()?;
        if let Some(txid) = send_payment(outputs, &self.pool_address, &client).await? {
            for member in unpaid_payout_members.iter() {
                if let Err(e) = database::set_txid_payment_member(&mut tx, member, &txid).await {
                    error!(failed_member = ?member);
                    error!(?unpaid_payout_members);
                    error!(?txid);
                    error!(?e);

                    bail!("A payment was sent but the database failed to update.");
                };
            }

            tx.commit().await?;

            info!(?txid, "Sent payment");
        }

        Ok(())
    }

    async fn keep_creating_payouts(&self, subsys: &SubsystemHandle) -> Result<()> {
        while !subsys.is_shutdown_requested() {
            if let Err(e) = self.new_payout().await {
                error!(error = ?e, "Failed to create new payout");
            }

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {},
                _ = tokio::time::sleep(Duration::from_secs(self.config.check_interval_in_secs)) => {}
            }
        }

        Ok(())
    }

    async fn keep_sending_payments(&self, subsys: &SubsystemHandle) -> Result<()> {
        while !subsys.is_shutdown_requested() {
            if let Err(e) = self.send_unsent_payouts().await {
                error!(error = ?e, "Failed to send payment");

                bail!("Failed to send payments");
            }

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {},
                _ = tokio::time::sleep(Duration::from_secs(self.config.send_interval_in_secs)) => {}
            }
        }

        Ok(())
    }
}

pub fn prepare_payment<'a>(
    payout_members: &Vec<PayoutMember>,
) -> Result<Vec<SendCurrencyOutput<'a>>> {
    let mut payout_members_map: HashMap<Address, Amount> = HashMap::new();
    for member in payout_members.into_iter() {
        payout_members_map
            .entry(member.identity_address.clone())
            .and_modify(|sum| *sum += member.reward)
            .or_insert(member.reward);
    }

    // let payment_vouts = payout_members
    //     .iter()
    //     .map(|pm| {
    //         (
    //             pm.identity_address,
    //             pm.iter().fold(Amount::ZERO, |acc, pm| acc + pm.reward),
    //         )
    //     })
    //     .collect::<HashMap<&Address, Amount>>();

    debug!("payment_vouts {:#?}", payout_members_map);

    let outputs = payout_members_map
        .iter()
        .map(|(address, amount)| {
            SendCurrencyOutput::new(None, amount, &address.to_string(), None, None)
        })
        .collect::<Vec<_>>();

    Ok(outputs)
}

pub async fn send_payment<'a>(
    outputs: Vec<SendCurrencyOutput<'a>>,
    pool_address: &Address,
    client: &Client,
) -> Result<Option<Txid>> {
    debug!(n_outputs = outputs.len(), ?outputs, "sending outputs");
    let opid = client.send_currency(&pool_address.to_string(), outputs, None, None)?;

    if let Some(txid) = wait_for_sendcurrency_finish(client, &opid).await? {
        return Ok(Some(txid));
    }

    Ok(None)
}

async fn wait_for_sendcurrency_finish(client: &Client, opid: &str) -> Result<Option<Txid>> {
    // from https://buildmedia.readthedocs.org/media/pdf/zcash/english-docs/zcash.pdf
    // status can be one of queued, executing, failed or success.
    // we should sleep if status is one of queued or executing
    // we should return when status is one of failed or success.
    loop {
        trace!("getting operation status: {}", &opid);
        let operation_status = client.z_get_operation_status(vec![opid])?;
        trace!("got operation status: {:?}", &operation_status);

        if let Some(Some(opstatus)) = operation_status.first() {
            if ["queued", "executing"].contains(&opstatus.status.as_ref()) {
                tokio::time::sleep(Duration::from_millis(100)).await;
                trace!("opid still executing");
                continue;
            }

            if let Some(txid) = &opstatus.result {
                trace!(
                    "there was an operation_status, operation was executed with status: {}",
                    opstatus.status
                );

                return Ok(Some(txid.txid));
            } else {
                error!("execution failed with status: {}", opstatus.status);
            }
        } else {
            trace!("there was NO operation_status");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[async_trait::async_trait]
impl IntoSubsystem<anyhow::Error> for Service {
    async fn run(self, subsys: SubsystemHandle) -> Result<()> {
        tokio::try_join!(
            self.keep_creating_payouts(&subsys),
            self.keep_sending_payments(&subsys)
        )?;

        Ok(())
    }
}
