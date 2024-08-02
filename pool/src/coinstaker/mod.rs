pub mod coinstaker;
mod config;
pub mod constants;
pub mod http;
#[cfg(feature = "mock")]
mod mock;
mod zmq;

pub use config::get_coin_configurations;
pub use config::Config;
pub use config::PayoutConfig;
pub use constants::StakerStatus;
