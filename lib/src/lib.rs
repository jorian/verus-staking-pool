use std::{convert::Infallible, str::FromStr};

use color_eyre::Report;
pub use rmp_serde::{Deserializer, Serializer};
use rust_decimal::Decimal;
pub use serde::{Deserialize, Serialize};
pub use sqlx::PgPool;
use vrsc_rpc::{
    bitcoin::{BlockHash, Txid},
    json::vrsc::{util::amount::serde::as_sat, Address, Amount},
    jsonrpc::serde_json,
};

pub mod chain;
pub mod configuration;
pub mod database;
pub mod payout;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../migrations");

#[derive(Debug, Clone)]
pub struct PayoutMember {
    pub blockhash: BlockHash,
    pub identityaddress: Address,
    pub reward: Amount,
    pub shares: Decimal,
    // fee is in basis points: 5% should be entered as 0.05
    pub fee: Amount,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Payload {
    pub command: String,
    pub data: serde_json::Value, //Option<String>,
}

impl Payload {
    pub fn new<I: Into<String>>(cmd: I, data: serde_json::Value) -> Self {
        let payload = Payload {
            command: cmd.into(),
            data,
        };

        payload
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StakeMember {
    pub identity_address: Address,
    pub shares: Decimal,
    // fee is in basis points: 5% should be entered as 0.05
    pub fee: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscriber {
    pub currencyid: Address,
    pub identity_address: Address,
    pub identity_name: String,
    pub discord_user_id: u64,
    pub bot_address: Address,
    pub min_payout: Amount,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stake {
    pub currencyid: Address,
    pub blockhash: BlockHash,
    pub mined_by: Address,
    pub pos_source_txid: Txid,
    pub pos_source_vout_num: u16,
    #[serde(with = "as_sat")]
    pub pos_source_amount: Amount,
    pub result: StakeResult,
    #[serde(with = "as_sat")]
    pub amount: Amount,
    pub blockheight: u64,
}

impl Stake {
    pub fn new(
        currencyid: Address,
        blockhash: BlockHash,
        mined_by: Address,
        pos_source_txid: Txid,
        pos_source_vout_num: u16,
        pos_source_amount: Amount,
        result: StakeResult,
        amount: Amount,
        blockheight: u64,
    ) -> Self {
        Self {
            currencyid,
            blockhash,
            mined_by,
            pos_source_txid,
            pos_source_vout_num,
            pos_source_amount,
            result,
            amount,
            blockheight,
        }
    }

    pub fn set_result(&mut self, result: &str) -> Result<(), Report> {
        self.result = StakeResult::from_str(result)?;

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StakeResult {
    Mature,
    Stale,
    Pending,
}

impl FromStr for StakeResult {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mature" => Ok(Self::Mature),
            "stale" => Ok(Self::Stale),
            "" => Ok(Self::Pending),
            _ => panic!(),
        }
    }
}

impl ToString for StakeResult {
    fn to_string(&self) -> String {
        match self {
            Self::Mature => "mature".to_string(),
            Self::Stale => "stale".to_string(),
            Self::Pending => "".to_string(),
        }
    }
}

// 62008467889825894230843329f6ce9d17c3944e:round478667:112_345_678
// impl FromStr for Stake {
//     type Err = Infallible; // gigo

//     fn from_str(s: &str) -> Result<Self, Self::Err> {
//         let mut parts = s.split(':');

//         let currencyid = parts.next().unwrap();
//         let blockheight = parts.next().unwrap();
//         let amount: Amount = Amount::from_sat(parts.next().unwrap().parse::<u64>().unwrap());

//         match &blockheight[5..] {
//             "Current" => Ok(Round {
//                 amount,
//                 currencyid: Address::from_str(currencyid).unwrap(),
//                 height: Height::Current(),
//             }),
//             x => Ok(Round {
//                 amount,
//                 currencyid: Address::from_str(currencyid).unwrap(),
//                 height: Height::BlockHeight(
//                     x.parse::<u64>().expect("an int representing a blockheight"),
//                 ),
//             }),
//         }
//     }
// }

// #[cfg(test)]
// mod tests {
//     use vrsc_rpc::json::vrsc::Amount;

//     use super::*;

//     #[test]
//     fn create_round() {
//         let round = Round::new(
//             Amount::ZERO,
//             Height::BlockHeight(123456),
//             Address::from_str("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq").unwrap(),
//         );

//         assert_eq!(
//             round.to_string(),
//             String::from("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq:round123456:0")
//         );
//     }

//     #[test]
//     fn create_current_round() {
//         let round = Round::new(
//             Amount::ZERO,
//             Height::Current(),
//             Address::from_str("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq").unwrap(),
//         );

//         assert_eq!(
//             round.to_string(),
//             String::from("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq:roundCurrent:0")
//         );
//     }

//     #[test]
//     fn parse_round_from_str() {
//         let s = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq:round123456:1";
//         let round = Round::from_str(s).unwrap();

//         assert_eq!(
//             Round::new(
//                 Amount::ONE_SAT,
//                 Height::BlockHeight(123456),
//                 Address::from_str("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq").unwrap(),
//             ),
//             round
//         );
//     }

//     #[test]
//     fn parse_current_round_from_str() {
//         let s = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq:roundCurrent:0";
//         let round = Round::from_str(s).unwrap();

//         assert_eq!(
//             Round::new(
//                 Amount::ZERO,
//                 Height::Current(),
//                 Address::from_str("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq").unwrap(),
//             ),
//             round
//         );
//     }
// }
