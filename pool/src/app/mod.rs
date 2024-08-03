use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    coinstaker::{
        self,
        coinstaker::{CoinStaker, CoinStakerMessage},
        get_coin_configurations,
    },
    config::Config,
    controller::Controller,
    http::HttpService,
    payout_service,
};
use anyhow::Result;
use secrecy::ExposeSecret;
use sqlx::{pool::PoolOptions, postgres::PgConnectOptions, PgPool};
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemBuilder, Toplevel};

pub struct App {
    pool: PgPool,
    config: Config,
}

#[cfg(not(feature = "mock"))]
impl App {
    pub async fn new(config: Config) -> Result<Self> {
        let pool: PgPool = PoolOptions::new().max_connections(20).connect_lazy_with(
            PgConnectOptions::new()
                .host(&config.database.host)
                .port(config.database.port)
                .username(&config.database.username)
                .database(&config.database.name)
                .password(config.database.password.expose_secret()),
        );

        Ok(Self { pool, config })
    }

    pub fn services(self) -> Result<Toplevel> {
        let coin_configs = get_coin_configurations()?;
        let mut coin_stakers = vec![];
        let mut coin_staker_payouts = vec![];
        let mut coin_staker_map = HashMap::new();
        for coin_config in coin_configs {
            let (tx, rx) = mpsc::channel::<CoinStakerMessage>(1024);
            let currency_id = coin_config.currency_id.clone();
            let coin_staker =
                CoinStaker::new(self.pool.clone(), coin_config.clone(), tx.clone(), rx)?;
            coin_stakers.push(coin_staker);

            let payout = payout_service::Service::new(
                coin_config.payout_config,
                self.pool.clone(),
                currency_id.clone(),
                coin_config.pool_address.clone(),
                coin_config.chain_config.clone(),
            );
            coin_staker_payouts.push((currency_id.clone(), payout));
            coin_staker_map.insert(currency_id, tx);
        }

        let http_service = HttpService {
            state: Arc::new(Controller {
                database: String::new(),
                coin_stakers: coin_staker_map,
            }),
            config: self.config.http,
        };

        let toplevel = Toplevel::new(|s| async move {
            s.start(SubsystemBuilder::new(
                "HttpService",
                http_service.into_subsystem(),
            ));

            for cs in coin_stakers {
                s.start(SubsystemBuilder::new(
                    format!("CoinStakerService.{}", cs.chain_id),
                    cs.into_subsystem(),
                ));
            }

            for (name, payout) in coin_staker_payouts {
                s.start(SubsystemBuilder::new(
                    format!("CoinStakerPayoutService.{name}"),
                    payout.into_subsystem(),
                ));
            }
        });

        Ok(toplevel)
    }
}

#[cfg(feature = "mock")]
impl App {
    pub async fn new(config: Config) -> Result<Self> {
        let pool: PgPool = PoolOptions::new().max_connections(1).connect_lazy_with(
            PgConnectOptions::new()
                .host(&config.database.host)
                .port(config.database.port)
                .username(&config.database.username)
                .database(&config.database.name)
                .password(config.database.password.expose_secret()),
        );

        Ok(Self { pool, config })
    }

    pub fn services(self) -> Result<Toplevel> {
        let pool = self.pool.clone();

        let (tx, rx) = mpsc::channel(128);

        let config_path: PathBuf = "coin_configs/vrsctest.toml".into();
        let config = config::Config::builder()
            .add_source(config::File::from(config_path.as_path()))
            .build()?
            .try_deserialize::<coinstaker::Config>()?;

        let coinstaker = CoinStaker::new(pool, config, tx, rx)?;

        Ok(Toplevel::new(|toplevel| async move {
            toplevel.start(SubsystemBuilder::new(
                "mock".to_string(),
                coinstaker.into_subsystem(),
            ));
        }))
    }
}
