use anyhow::{Context, Result};
use rust_decimal::{prelude::FromPrimitive, prelude::ToPrimitive, Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
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
    pub fn new(stake: &Stake, workers: Vec<Worker>, pool_fee_discount: Decimal) -> Result<Self> {
        // get work and fee by round
        let amount = Decimal::from_f64(stake.amount.as_vrsc())
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
                stake.currency_address.clone(),
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
        if let Some(fee) = stake.amount.checked_sub(reward_sum) {
            if fee > Amount::ZERO {
                trace!(
                        "pool_fee: there is a difference of {pool_fee} between the staked amount and the amount to pay out"
                    );

                pool_fee += fee;
            }
        }

        Ok(Self {
            currency_address: stake.currency_address.clone(),
            block_hash: stake.block_hash,
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
    pub currency_address: Address,
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
        currency_address: Address,
        block_hash: BlockHash,
        block_height: u64,
        identity_address: Address,
        reward: Amount,
        shares: Decimal,
        fee: Amount,
    ) -> Self {
        Self {
            currency_address,
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

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use std::str::FromStr;

    use crate::database;

    use super::*;

    const _VRSC: &str = "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV";
    const _VRSCTEST: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
    const ALICE: &str = "iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU";
    const BOB: &str = "iGLN3bFv6uY2HAgQgVwiGriTRgQmTyJrwi";
    // const CHARLIE: &str = "RDebEHgiTFDRDUN5Uisx7ntUuRdRJHt6SK";
    // const DIRK: &str = "RSTWA7QcQaEbhS4iJha2p1b5eYvUPpVXGP";
    // const EMILY: &str = "RRVdSds5Zck6YnhYgchL8qCKqARhob64vk";
    const _POOL_ADDRESS: &str = "iBnKXQnD1BFyvE8V4UVr4UKQz8h7FqfVu9";

    #[sqlx::test(fixtures("stakes.sql"), migrator = "crate::MIGRATOR")]
    fn single_member(pool: PgPool) -> sqlx::Result<()> {
        let stake = database::get_stake(
            &pool,
            &Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap(),
            513251,
        )
        .await
        .unwrap()
        .unwrap();

        let mut stake_members = vec![];
        stake_members.push(Worker {
            identity_address: Address::from_str(ALICE).unwrap(),
            shares: Decimal::from_f64(123.456).unwrap(),
            fee: Decimal::from_f32(0.01).unwrap(),
        });

        let payout = Payout::new(&stake, stake_members, Decimal::ZERO).unwrap();

        let mut to_test_against = vec![];

        to_test_against.push(PayoutMember {
            currency_address: Address::from_str(_VRSC).unwrap(),
            block_hash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
            )
            .unwrap(),
            block_height: 513251,
            identity_address: Address::from_str(ALICE).unwrap(),
            reward: Amount::from_sat(594_099_000),
            shares: Decimal::from_f64(123.456).unwrap(),
            fee: Amount::from_sat(6_001_000),
            txid: None,
        });

        assert_eq!(payout.members, to_test_against);

        Ok(())
    }

    #[sqlx::test(fixtures("stakes"), migrator = "crate::MIGRATOR")]
    fn double_member(pool: PgPool) -> sqlx::Result<()> {
        let stake = database::get_stake(
            &pool,
            &Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap(),
            513251,
        )
        .await
        .unwrap()
        .unwrap();

        let mut stake_members = vec![];
        stake_members.push(Worker {
            identity_address: Address::from_str(ALICE).unwrap(),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Decimal::from_f32(0.01).unwrap(),
        });

        stake_members.push(Worker {
            identity_address: Address::from_str(BOB).unwrap(),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Decimal::from_f32(0.01).unwrap(),
        });

        let payout = Payout::new(&stake, stake_members, Decimal::ZERO).unwrap();

        let alice = PayoutMember {
            currency_address: Address::from_str(_VRSC).unwrap(),
            block_hash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
            )
            .unwrap(),
            block_height: 513251,
            identity_address: Address::from_str(ALICE).unwrap(),
            reward: Amount::from_sat(297_049_500),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Amount::from_sat(3_000_500),
            txid: None,
        };

        assert!(payout.members.contains(&alice));

        let bob = PayoutMember {
            currency_address: Address::from_str(_VRSC).unwrap(),
            block_hash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
            )
            .unwrap(),
            block_height: 513251,
            identity_address: Address::from_str(ALICE).unwrap(),
            reward: Amount::from_sat(297_049_500),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Amount::from_sat(3_000_500),
            txid: None,
        };

        assert!(payout.members.contains(&bob));

        Ok(())
    }
} /*

      #[sqlx::test(fixtures("stakes"), migrator = "crate::MIGRATOR")]
      #[traced_test]
      fn triple_member(pool: PgPool) -> sqlx::Result<()> {
          let stake = database::get_stake(&pool, "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV", 513251)
              .await
              .unwrap()
              .unwrap();

          let mut stake_members = vec![];
          stake_members.push(Worker {
              identity_address: Address::from_str(ALICE).unwrap(),
              shares: Decimal::from_f64(5.0).unwrap(),
              fee: Decimal::from_f32(0.01).unwrap(),
          });

          stake_members.push(Worker {
              identity_address: Address::from_str(BOB).unwrap(),
              shares: Decimal::from_f64(5.0).unwrap(),
              fee: Decimal::from_f32(0.01).unwrap(),
          });

          stake_members.push(Worker {
              identity_address: Address::from_str(CHARLIE).unwrap(),
              shares: Decimal::from_f64(5.0).unwrap(),
              fee: Decimal::from_f32(0.01).unwrap(),
          });

          let payout = Payout::new(
              &stake,
              Decimal::ZERO,
              stake_members,
              Address::from_str(POOL_ADDRESS).unwrap(),
          )
          .unwrap();

          let alice = PayoutMember {
              block_hash: block_hash::from_str(
                  "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
              )
              .unwrap(),
              blockheight: 513251,
              identityaddress: Address::from_str(ALICE).unwrap(),
              reward: Amount::from_sat(198_033_000),
              shares: Decimal::from_f64(5.0).unwrap(),
              fee: Amount::from_sat(2_000_333),
              txid: None,
          };

          assert!(payout.members.contains(&alice));

          let bob = PayoutMember {
              block_hash: block_hash::from_str(
                  "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
              )
              .unwrap(),
              blockheight: 513251,
              identityaddress: Address::from_str(ALICE).unwrap(),
              reward: Amount::from_sat(198_033_000),
              shares: Decimal::from_f64(5.0).unwrap(),
              fee: Amount::from_sat(2_000_333),
              txid: None,
          };

          assert!(payout.members.contains(&bob));

          let charlie = PayoutMember {
              block_hash: block_hash::from_str(
                  "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
              )
              .unwrap(),
              blockheight: 513251,
              identityaddress: Address::from_str(ALICE).unwrap(),
              reward: Amount::from_sat(198_033_000),
              shares: Decimal::from_f64(5.0).unwrap(),
              fee: Amount::from_sat(2_000_333),
              txid: None,
          };

          assert!(payout.members.contains(&charlie));
          assert_eq!(payout.pool_fee_amount, Amount::from_sat(6_001_000));

          Ok(())
      }

      #[sqlx::test(fixtures("stakes"), migrator = "crate::MIGRATOR")]
      #[traced_test]
      fn random_numbers(pool: PgPool) -> sqlx::Result<()> {
          let stake = database::get_stake(&pool, "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV", 22222)
              .await
              .unwrap()
              .unwrap();

          let mut stake_members = vec![];
          stake_members.push(Worker {
              identity_address: Address::from_str(ALICE).unwrap(),
              shares: Decimal::from_i64(118058969151204).unwrap(),
              fee: Decimal::from_f32(0.05).unwrap(),
          });

          stake_members.push(Worker {
              identity_address: Address::from_str(BOB).unwrap(),
              shares: Decimal::from_i64(226369800917454).unwrap(),
              fee: Decimal::from_f32(0.05).unwrap(),
          });

          let payout = Payout::new(
              &stake,
              Decimal::ZERO,
              stake_members,
              Address::from_str(POOL_ADDRESS).unwrap(),
          )
          .unwrap();

          debug!("{payout:#?}");

          let alice = PayoutMember {
              block_hash: block_hash::from_str(
                  "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e1",
              )
              .unwrap(),
              blockheight: 22222,

              identityaddress: Address::from_str(ALICE).unwrap(),
              reward: Amount::from_sat(2_500_831_271),
              shares: Decimal::from_i64(118_058_969_151_204).unwrap(),
              fee: Amount::from_sat(131_622_698),
              txid: None,
          };

          assert!(payout.members.contains(&alice));

          let bob = PayoutMember {
              block_hash: block_hash::from_str(
                  "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e1",
              )
              .unwrap(),
              blockheight: 22222,

              identityaddress: Address::from_str(BOB).unwrap(),
              reward: Amount::from_sat(4_795_168_728),
              shares: Decimal::from_i64(226_369_800_917_454).unwrap(),
              fee: Amount::from_sat(252_377_302),
              txid: None,
          };

          assert!(payout.members.contains(&alice));
          assert!(payout.members.contains(&bob));

          assert_eq!(payout.pool_fee_amount, Amount::from_sat(384_000_001));

          Ok(())
      }
  }

      #[traced_test]
      #[test]
      fn two_participants() {
          let mut members = vec![];
          members.push(PayoutMember {
              identityaddress: Address::from_str(ALICE).unwrap(),
              shares: Decimal::from_f32(50.0).unwrap(),
              fee: Decimal::from_f32(0.05).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(BOB).unwrap(),
              shares: Decimal::from_f32(50.0).unwrap(),
              fee: Decimal::from_f32(0.05).unwrap(),
          });

          let payout = Payout::new(
              Amount::from_sat(600_000_000),
              // Amount::from_sat(10000),
              Decimal::ZERO,
              members,
              Address::from_str(BOT_ADDRESS).unwrap(),
              // client(),
          )
          .unwrap();

          let mut final_calc = HashMap::new();
          final_calc.insert(
              Address::from_str(ALICE).unwrap(),
              Amount::from_sat(284_995_250),
          );

          final_calc.insert(
              Address::from_str(BOB).unwrap(),
              Amount::from_sat(284_995_250),
          );

          final_calc.insert(
              Address::from_str(BOT_ADDRESS).unwrap(),
              Amount::from_sat(29_999_500),
          );

          assert_eq!(payout.final_calc, final_calc);
      }

      #[test]
      fn two_participants_one_with_zero_work() {
          // zero work should not be here already, as it is filtered out in Coinstaker
          let mut members = vec![];
          members.push(PayoutMember {
              identityaddress: Address::from_str(ALICE).unwrap(),
              shares: Decimal::ZERO,
              fee: Decimal::from_f32(0.05).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(CHARLIE).unwrap(),
              shares: Decimal::from_f32(100.0).unwrap(),
              fee: Decimal::from_f32(0.05).unwrap(),
          });

          let payout = Payout::new(
              Amount::from_sat(600_000_000),
              // Amount::from_sat(10000),
              Decimal::ZERO,
              members,
              Address::from_str(BOT_ADDRESS).unwrap(),
              // client(),
          )
          .unwrap();

          let mut final_calc = HashMap::new();

          final_calc.insert(
              Address::from_str(CHARLIE).unwrap(),
              Amount::from_sat(569_990_500),
          );

          final_calc.insert(
              Address::from_str(BOT_ADDRESS).unwrap(),
              Amount::from_sat(29_999_500),
          );

          assert_eq!(payout.final_calc, final_calc);
      }

      #[test]
      fn two_participants_uneven_amount() {
          let mut members = vec![];
          members.push(PayoutMember {
              identityaddress: Address::from_str(ALICE).unwrap(),
              shares: Decimal::from_f32(50.0).unwrap(),
              fee: Decimal::from_f32(0.05).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(CHARLIE).unwrap(),
              shares: Decimal::from_f32(50.0).unwrap(),
              fee: Decimal::from_f32(0.05).unwrap(),
          });

          let payout = Payout::new(
              Amount::from_sat(33333),
              // Amount::from_sat(10000),
              Decimal::ZERO,
              members,
              Address::from_str(BOT_ADDRESS).unwrap(),
              // client(),
          )
          .unwrap();

          let mut final_calc = HashMap::new();
          final_calc.insert(Address::from_str(ALICE).unwrap(), Amount::from_sat(11083));

          final_calc.insert(Address::from_str(CHARLIE).unwrap(), Amount::from_sat(11083));

          final_calc.insert(
              Address::from_str(BOT_ADDRESS).unwrap(),
              Amount::from_sat(1167),
          );

          assert_eq!(payout.final_calc, final_calc);
      }

      #[test]
      fn two_participants_uneven_amount_zero_fee() {
          let mut members = vec![];
          members.push(PayoutMember {
              identityaddress: Address::from_str(ALICE).unwrap(),
              shares: Decimal::from_f32(50.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(CHARLIE).unwrap(),
              shares: Decimal::from_f32(50.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          let payout = Payout::new(
              Amount::from_sat(33333),
              // Amount::from_sat(0),
              Decimal::ZERO,
              members,
              Address::from_str(BOT_ADDRESS).unwrap(),
              // client(),
          )
          .unwrap();

          let mut final_calc = HashMap::new();
          final_calc.insert(Address::from_str(ALICE).unwrap(), Amount::from_sat(16666));

          final_calc.insert(Address::from_str(CHARLIE).unwrap(), Amount::from_sat(16666));

          final_calc.insert(Address::from_str(BOT_ADDRESS).unwrap(), Amount::from_sat(1));

          assert_eq!(payout.final_calc, final_calc);
      }

      #[test]
      fn three_participants_uneven_amount() {
          let mut members = vec![];

          members.push(PayoutMember {
              identityaddress: Address::from_str(ALICE).unwrap(),
              shares: Decimal::from_f32(100.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(CHARLIE).unwrap(),
              shares: Decimal::from_f32(100.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(BOB).unwrap(),
              shares: Decimal::from_f32(100.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          let payout = Payout::new(
              Amount::from_sat(5),
              // Amount::from_sat(0),
              Decimal::ZERO,
              members,
              Address::from_str(BOT_ADDRESS).unwrap(),
              // client(),
          )
          .unwrap();

          let mut final_calc = HashMap::new();
          final_calc.insert(Address::from_str(ALICE).unwrap(), Amount::from_sat(1));

          final_calc.insert(Address::from_str(CHARLIE).unwrap(), Amount::from_sat(1));

          final_calc.insert(Address::from_str(BOB).unwrap(), Amount::from_sat(1));

          final_calc.insert(Address::from_str(BOT_ADDRESS).unwrap(), Amount::from_sat(2));

          assert_eq!(payout.final_calc, final_calc);
      }

      #[test]
      fn five_participants() {
          let mut members = vec![];

          members.push(PayoutMember {
              identityaddress: Address::from_str(ALICE).unwrap(),
              shares: Decimal::from_f32(1.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(BOB).unwrap(),
              shares: Decimal::from_f32(1.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(CHARLIE).unwrap(),
              shares: Decimal::from_f32(1.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(DIRK).unwrap(),
              shares: Decimal::from_f32(2.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(EMILY).unwrap(),
              shares: Decimal::from_f32(1.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          let payout = Payout::new(
              Amount::from_sat(600_000_000),
              // Amount::from_sat(0),
              Decimal::ZERO,
              members,
              Address::from_str(BOT_ADDRESS).unwrap(),
              // client(),
          )
          .unwrap();

          let mut final_calc = HashMap::new();

          final_calc.insert(
              Address::from_str(ALICE).unwrap(),
              Amount::from_sat(100_000_000),
          );

          final_calc.insert(
              Address::from_str(BOB).unwrap(),
              Amount::from_sat(100_000_000),
          );

          final_calc.insert(
              Address::from_str(CHARLIE).unwrap(),
              Amount::from_sat(100_000_000),
          );

          final_calc.insert(
              Address::from_str(DIRK).unwrap(),
              Amount::from_sat(200_000_000),
          );

          final_calc.insert(
              Address::from_str(EMILY).unwrap(),
              Amount::from_sat(100_000_000),
          );

          assert_eq!(payout.final_calc, final_calc);
      }

      #[test]
      fn payout_too_low() {
          let payout = Payout::new(
              Amount::from_sat(5000),
              // Amount::from_sat(10000),
              Decimal::ZERO,
              vec![],
              Address::from_str(BOT_ADDRESS).unwrap(),
              // client(),
          );

          assert!(payout.is_err());

          let payout = Payout::new(
              Amount::from_sat(100_000_000),
              // Amount::from_sat(10000),
              Decimal::ZERO,
              vec![],
              Address::from_str(BOT_ADDRESS).unwrap(),
              // client(),
          );

          assert!(payout.is_ok());
      }

      #[test]
      #[traced_test]
      fn test_real_scenario_zero_fee() {
          let mut members = vec![];

          members.push(PayoutMember {
              identityaddress: Address::from_str(ALICE).unwrap(),
              shares: Decimal::from_f32(3934161183517936.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          members.push(PayoutMember {
              identityaddress: Address::from_str(BOB).unwrap(),
              shares: Decimal::from_f32(3840965826963724.0).unwrap(),
              fee: Decimal::from_f32(0.0).unwrap(),
          });

          let payout = Payout::new(
              Amount::from_sat(76800000000),
              // Amount::from_sat(10000),
              Decimal::ZERO,
              members,
              Address::from_str(BOT_ADDRESS).unwrap(),
              // client(),
          )
          .unwrap();

          let mut final_calc = HashMap::new();
          final_calc.insert(
              Address::from_str(ALICE).unwrap(),
              Amount::from_sat(38860269595),
          );

          final_calc.insert(
              Address::from_str(BOB).unwrap(),
              Amount::from_sat(37939720404),
          );

          final_calc.insert(Address::from_str(BOT_ADDRESS).unwrap(), Amount::from_sat(1));

          assert_eq!(payout.final_calc, final_calc);
      }
  }
  */
