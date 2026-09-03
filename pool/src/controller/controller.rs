use std::collections::HashMap;

use crate::coinstaker::CoinStakerHandle;
use vrsc_rpc::json::vrsc::Address;

pub struct Controller {
    pub database: String,
    pub coin_stakers: HashMap<Address, CoinStakerHandle>,
}

impl Controller {
    pub fn version(&self) -> String {
        format!("{}", 0.1)
    }
}
