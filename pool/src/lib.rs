pub mod app;
pub mod coinstaker;
pub mod config;
pub mod controller;
pub mod database;
pub mod http;
pub mod payout;
pub mod util;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("sql/migrations");
