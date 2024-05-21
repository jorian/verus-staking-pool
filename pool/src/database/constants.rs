use std::str::FromStr;

use vrsc_rpc::json::vrsc::{Address, Amount};

use crate::coinstaker::{constants::Staker, StakerStatus};

pub struct DbStaker {
    pub(super) currency_address: String,
    pub(super) identity_address: String,
    pub(super) identity_name: String,
    pub(super) min_payout: i64,
    pub(super) status: StakerStatus,
}

impl TryFrom<DbStaker> for Staker {
    type Error = sqlx::Error;

    fn try_from(value: DbStaker) -> Result<Self, Self::Error> {
        let staker = Self {
            currency_address: Address::from_str(&value.currency_address)
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            identity_address: Address::from_str(&value.identity_address)
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            identity_name: value.identity_name,
            min_payout: Amount::from_sat(value.min_payout as u64),
            status: value.status,
        };

        Ok(staker)
    }
}
