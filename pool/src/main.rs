use std::collections::HashMap;

use coinstaker::{CoinStaker, CoinStakerMessage};
use color_eyre::Report;
use configuration::get_app_config;
use futures::{future::join_all, stream::FuturesUnordered};

use poollib::{configuration::get_coin_configurations, PgPool};
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, Level};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub mod coinstaker;
mod configuration;
mod payoutmanager;

#[tokio::main]
async fn main() -> Result<(), Report> {
    logging_setup();

    let mut controller = Controller {
        senders: HashMap::new(),
    };

    if let Err(e) = controller.start().await {
        error!("controller error: {:?}", e);
    }

    Ok(())
}

#[derive(Debug)]
pub struct Controller {
    senders: HashMap<String, mpsc::Sender<CoinStakerMessage>>,
}

impl Controller {
    async fn start(&mut self) -> Result<(), Report> {
        info!("starting controller");

        let config = get_app_config().await?;
        let pg_url = config.database.connection_string();
        let pool = PgPool::connect_lazy(&pg_url).expect("a database pool");

        let coin_configs = get_coin_configurations()?;
        debug!("{:?}", &coin_configs);
        let handles = FuturesUnordered::new();

        for coin_config in coin_configs {
            let (cs_tx, cs_rx) = mpsc::channel::<CoinStakerMessage>(100);
            let coin_staker =
                CoinStaker::new(pool.clone(), coin_config, cs_rx, cs_tx.clone()).await;

            let name = coin_staker.chain.currencyid.clone();
            self.senders.insert(name.to_string().clone(), cs_tx.clone());

            handles.push(tokio::spawn(async move {
                if let Err(e) = coinstaker::run(coin_staker).await {
                    // TODO notifications: https://github.com/jorian/verus-staking-pool/issues/10
                    error!("Error in coinstaker {}: {:?}", name, e);
                }
            }));
        }
        trace!("started csm");

        join_all(handles).await;

        Ok(())
    }
}

fn logging_setup() {
    if std::env::var("RUST_LIB_BACKTRACE").is_err() {
        std::env::set_var("RUST_LIB_BACKTRACE", "1")
    }

    let _ = color_eyre::install();

    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "vrsc-rpc=debug,pool=trace")
    }

    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    let file_appender = tracing_appender::rolling::hourly("./logs", "trace");
    // let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt::Layer::default())
        .with(
            fmt::Layer::new()
                // .json()
                // .with_ansi(false)
                .with_writer(file_appender.with_max_level(Level::TRACE)),
        )
        .try_init()
        .expect("tracing");

    info!("logging enabled");
}
