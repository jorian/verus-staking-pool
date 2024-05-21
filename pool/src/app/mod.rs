use std::{collections::HashMap, sync::Arc};

use crate::{
    coinstaker::{
        coinstaker::{CoinStaker, CoinStakerMessage},
        get_coin_configurations,
    },
    config::Config,
    controller::Controller,
    http::HttpService,
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
        let chain_configs = get_coin_configurations()?;
        let mut coin_stakers = vec![];
        let mut coin_staker_map = HashMap::new();
        for chain_config in chain_configs {
            let (tx, rx) = mpsc::channel::<CoinStakerMessage>(512);
            let chain_id = chain_config.chain_id.clone();
            coin_stakers.push(CoinStaker::new(
                self.pool.clone(),
                chain_config,
                tx.clone(),
                rx,
            )?);
            coin_staker_map.insert(chain_id, tx);
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
                    format!("CoinStakerService.{}", cs.chain_id.to_string()),
                    cs.into_subsystem(),
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
        Ok(Toplevel::new(|_| async move {}))
    }
}
