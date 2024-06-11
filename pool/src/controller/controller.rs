use std::collections::HashMap;

use crate::coinstaker::coinstaker::CoinStakerMessage;
use tokio::sync::mpsc;
use vrsc_rpc::json::vrsc::Address;

pub struct Controller {
    pub database: String,
    pub coin_stakers: HashMap<Address, mpsc::Sender<CoinStakerMessage>>,
}

impl Controller {
    pub fn version(&self) -> String {
        format!("{}", 0.1)
    }
}
