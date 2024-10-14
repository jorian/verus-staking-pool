use std::fmt::Display;

use anyhow::Result;

use serde::Serialize;
use url::Url;
use vrsc_rpc::{
    bitcoin::BlockHash,
    json::vrsc::{util::amount::serde::as_sat, Address, Amount},
};

use super::constants::Stake;

// send webhook message to registered endpoints
#[derive(Debug)]
pub struct Webhook {
    client: reqwest::Client,
    endpoints: Vec<Url>,
}

impl Webhook {
    pub fn new(endpoints: Vec<Url>) -> Result<Self> {
        let client = reqwest::ClientBuilder::new().build()?;

        Ok(Self { client, endpoints })
    }

    pub async fn send(&self, msg: WebhookMessage) -> Result<()> {
        for endpoint in self.endpoints.iter() {
            self.client
                .post(endpoint.clone().join("/webhook").unwrap())
                .json(&WebhookBody::from(msg.clone()))
                .send()
                .await?;
        }

        Ok(())
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum WebhookMessage {
    StakeFound {
        currency_address: Address,
        currency_name: String,
        hash: BlockHash,
        height: u64,
        found_by: Address,
        #[serde(with = "as_sat")]
        amount: Amount,
    },
    StakeMatured {
        hash: BlockHash,
        height: u64,
    },
    NewStaker {
        identity_address: Address,
        identity_name: String,
    },
    LeavingStaker {
        identity_address: Address,
        identity_name: String,
    },
}

impl WebhookMessage {
    pub fn new_stake(currency_name: String, stake: &Stake) -> Self {
        Self::StakeFound {
            currency_address: stake.currency_address.clone(),
            currency_name,
            hash: stake.block_hash,
            height: stake.block_height,
            found_by: stake.found_by.clone(),
            amount: stake.amount,
        }
    }
}

impl Display for WebhookMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebhookMessage::StakeFound { .. } => write!(f, "stake_found"),
            WebhookMessage::StakeMatured { .. } => write!(f, "stake_matured"),
            WebhookMessage::NewStaker { .. } => write!(f, "new_staker"),
            WebhookMessage::LeavingStaker { .. } => write!(f, "leaving_staker"),
        }
    }
}

#[derive(Serialize)]
pub struct WebhookBody {
    message: String,
    data: String,
}

impl From<WebhookMessage> for WebhookBody {
    fn from(value: WebhookMessage) -> Self {
        Self {
            message: value.to_string(),
            data: serde_json::to_string(&value).unwrap(),
        }
    }
}
