pub mod coinstaker;
mod config;
pub mod constants;
pub mod http;
mod zmq;

pub use config::get_coin_configurations;
pub use constants::StakerStatus;
// spawns a ZMQ listener that listens for new blocks
