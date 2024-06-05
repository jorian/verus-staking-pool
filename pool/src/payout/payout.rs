use anyhow::{Context, Result};
use rust_decimal::{prelude::FromPrimitive, prelude::ToPrimitive, Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, trace};
use vrsc_rpc::{
    bitcoin::{BlockHash, Txid},
    json::vrsc::{util::amount::serde::as_sat, Address, Amount},
};

use crate::coinstaker::constants::Stake;

pub struct Payout {
    /// Currency for which the payout is generated
    pub currency_address: Address,
    pub block_hash: BlockHash,
    pub block_height: u64,
    pub total_work: Decimal,
    /// The staked reward that is to be paid out to workers.
    /// Should be the sum of `fee` + `paid`
    pub amount: Amount,
    /// The difference between `amount` and the rewards for every payoutmember is the pool fee
    pub fee: Amount,
    /// The amount paid out to workers
    pub paid: Amount,
    /// All the workers that participated in this round
    pub members: Vec<PayoutMember>,
}

impl Payout {
    /// A payout is the translation from work that workers put in to how much every worker
    /// gets from the staked amount, dividing the pie into every worker's fair share.
    ///
    /// First we need to determine the fee of every staker. Since the pool can set a
    /// fee discount and every staker has an individual fee percentage, it is different
    /// for every staker.
    ///
    ///
    pub fn new(
        stake: &Stake,
        workers: Vec<Worker>,
        pool_fee_discount: Decimal,
        pool_identity_address: Address,
    ) -> Result<Self> {
        // get work and fee by round
        let mut amount = Decimal::from_f64(stake.amount.as_vrsc())
            .context("Could not create Decimal from stake amount")?;
        let mut pool_fee = Amount::ZERO;

        let sum_of_shares = workers
            .iter()
            .fold(Decimal::ZERO, |acc, member| acc + member.shares);

        let mut payout_members = vec![];
        for worker in workers {
            if worker.shares == Decimal::ZERO {
                debug!("a worker with 0 shares was included");
                continue;
            }

            let worker_fee = worker
                .fee
                .checked_sub(pool_fee_discount)
                .unwrap_or(Decimal::ZERO);

            let portion = sum_of_shares / worker.shares;

            let amount_share = amount
                .checked_div(portion)
                .context("Could not determine amount for staker (div failed)")?;

            let amount_fee = amount_share
                .checked_mul(worker_fee)
                .unwrap_or(Decimal::ZERO);

            let net_amount_share = amount_share
                .checked_sub(amount_fee)
                .unwrap_or(amount_share)
                .round_dp_with_strategy(8, RoundingStrategy::ToZero);

            payout_members.push(PayoutMember::new(
                stake.block_hash,
                stake.block_height,
                worker.identity_address,
                Amount::from_vrsc(
                    net_amount_share
                        .to_f64()
                        .context("Failed to convert net amount share to `Amount`")?,
                )?,
                worker.shares,
                Amount::from_vrsc(
                    amount_fee
                        .to_f64()
                        .context("Failed to convert fee amount to `Amount`")?,
                )?,
            ));
        }

        let reward_sum = payout_members
            .iter()
            .fold(Amount::ZERO, |acc, member| acc + member.reward);

        debug!("sum of calculated rewards: {}", reward_sum);
        debug!("initial amount: {}", stake.amount.as_sat());

        // the difference is added to the pool fee
        if let Some(fee) = stake.amount.clone().checked_sub(reward_sum) {
            if fee > Amount::ZERO {
                trace!(
                        "pool_fee: there is a difference of {pool_fee} between the staked amount and the amount to pay out"
                    );

                pool_fee += fee;
            }
        }

        Ok(Self {
            currency_address: stake.currency_address.clone(),
            block_hash: stake.block_hash.clone(),
            block_height: stake.block_height,
            total_work: sum_of_shares,
            amount: stake.amount,
            fee: pool_fee,
            paid: reward_sum,
            members: payout_members,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayoutMember {
    pub block_hash: BlockHash,
    pub block_height: u64,
    pub identity_address: Address,
    #[serde(with = "as_sat")]
    pub reward: Amount,
    pub shares: Decimal,
    // fee is in basis points: 5% should be entered as 0.05
    #[serde(with = "as_sat")]
    pub fee: Amount,
    pub txid: Option<Txid>,
}

impl PayoutMember {
    fn new(
        block_hash: BlockHash,
        block_height: u64,
        identity_address: Address,
        reward: Amount,
        shares: Decimal,
        fee: Amount,
    ) -> Self {
        Self {
            block_hash,
            block_height,
            identity_address,
            reward,
            shares,
            fee,
            txid: None,
        }
    }
}

pub struct Worker {
    pub identity_address: Address,
    pub shares: Decimal,
    // fee is in basis points: 5% should be entered as 0.05
    pub fee: Decimal,
}
