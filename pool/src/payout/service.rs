use std::time::Duration;

use anyhow::Result;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemHandle};
use tracing::{error, info};
use vrsc_rpc::json::vrsc::Address;

use crate::{coinstaker::PayoutConfig as PayoutServiceConfig, database};

use super::payout::Payout;

pub struct Service {
    database: PgPool,
    config: PayoutServiceConfig,
    chain_id: Address,
}

impl Service {
    pub fn new(config: PayoutServiceConfig, database: PgPool, chain_id: Address) -> Self {
        Self {
            database,
            config,
            chain_id,
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

    async fn keep_creating_payouts(&self) -> Result<()> {
        loop {
            self.new_payout().await?;

            tokio::time::sleep(Duration::from_secs(self.config.check_interval_in_secs)).await;
        }
    }

    async fn keep_sending_payouts(&self) -> Result<()> {
        loop {
            // TODO send payment
            tokio::time::sleep(Duration::from_secs(self.config.send_interval_in_secs)).await;
        }
    }
}

#[async_trait::async_trait]
impl IntoSubsystem<anyhow::Error> for Service {
    async fn run(self, subsys: SubsystemHandle) -> Result<()> {
        while !subsys.is_shutdown_requested() {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    info!("PayoutService shutting down")
                }
                Err(e) = self.keep_creating_payouts() => {
                    error!("An error occured while doing payouts: {:?}", e);
                }
                Err(e) = self.keep_sending_payouts() => {
                    error!("An error occured while sending payouts: {:?}", e);
                }
            }
        }

        Ok(())
    }
}
