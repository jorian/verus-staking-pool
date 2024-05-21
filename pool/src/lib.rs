use anyhow::Result;
use axum::async_trait;
use vrsc_rpc::json::Block;

pub mod app;
pub mod coinstaker;
pub mod config;
pub mod controller;
pub mod database;
pub mod http;
pub mod util;

#[async_trait]
pub trait Verus {
    async fn get_block() -> Result<Block>;
}
