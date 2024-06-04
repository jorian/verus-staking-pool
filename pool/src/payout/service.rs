use std::time::Duration;

use anyhow::Result;
use sqlx::PgPool;
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemHandle};
use tracing::{error, info};

use crate::coinstaker::PayoutConfig as PayoutServiceConfig;

pub struct Service {
    database: PgPool,
    config: PayoutServiceConfig,
}

impl Service {
    pub fn new(config: PayoutServiceConfig, database: PgPool) -> Self {
        Self { database, config }
    }

    async fn new_payout(&self) -> Result<()> {
        Ok(())
    }

    async fn keep_creating_payouts(&self) -> Result<()> {
        loop {
            self.new_payout().await?;

            tokio::time::sleep(Duration::from_secs(self.config.interval_in_secs)).await;
        }
    }

    async fn keep_sending_payouts(&self) -> Result<()> {
        loop {
            self.new_payout().await?;

            tokio::time::sleep(Duration::from_secs(self.config.interval_in_secs)).await;
        }
    }
}

#[async_trait::async_trait]
impl IntoSubsystem<anyhow::Error> for Service {
    async fn run(self, subsys: SubsystemHandle) -> Result<()> {
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

        Ok(())
    }
}
