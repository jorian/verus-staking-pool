use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct StakingSupply {
    pub staker: f64,
    pub pool: f64,
    pub network: f64,
}
