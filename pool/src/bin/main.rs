use std::time::Duration;

use pool::{app::App, config::app_config};
use tracing::{debug, info, trace, Level};
use tracing_subscriber::{
    fmt::{self, writer::MakeWriterExt},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let filter_layer = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;

    let file_appender = tracing_appender::rolling::hourly("./logs", "error");

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt::Layer::default())
        .with(
            fmt::Layer::new()
                .json()
                .with_writer(file_appender.with_min_level(Level::DEBUG)),
        )
        .init();

    trace!("logging enabled");

    let config = app_config().await?;

    let app = App::new(config).await?;
    let services = app.services()?;

    info!("starting services");
    services
        .catch_signals()
        .handle_shutdown_requests(Duration::from_millis(1000))
        .await
        .map_err(Into::into)
}
