#[cfg(feature = "mock")]
use std::net::{IpAddr, Ipv4Addr};

#[cfg(feature = "mock")]
use pool::config::{AppConfig, Config, DbConfig, HttpConfig};
#[cfg(feature = "mock")]
use secrecy::Secret;

extern crate pool;

#[cfg(feature = "mock")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config {
        application: AppConfig {
            enable_tracing: true,
            trace_level: "TRACE".to_string(),
        },
        database: DbConfig {
            username: "test".to_string(),
            password: Secret::new("test".to_string()),
            port: 51111,
            host: "http://localhost".to_string(),
            name: "test".to_string(),
        },
        http: HttpConfig {
            host: std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 51112,
        },
    };

    let app = pool::app::App::new(config).await?;
    let services = app.services()?;

    // todo delete test database

    Ok(())
}
