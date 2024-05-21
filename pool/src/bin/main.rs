use std::time::Duration;

use pool::{app::App, config::app_config};
use tracing::{info, trace};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt()
        .with_file(true)
        .with_line_number(true)
        .with_env_filter(EnvFilter::from_default_env())
        .compact()
        .init();

    trace!("logging enabled");

    let config = app_config().await?;
    let conn_string = config.database.connection_string();
    info!("{conn_string}");

    let app = App::new(config).await?;
    let services = app.services()?;

    info!("starting services");
    services
        .catch_signals()
        .handle_shutdown_requests(Duration::from_millis(1000))
        .await
        .map_err(Into::into)
}
