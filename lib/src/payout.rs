use std::fmt::Display;

use color_eyre::Report;
use rust_decimal::{
    prelude::{FromPrimitive, ToPrimitive},
    Decimal,
};
use tracing::*;
use vrsc_rpc::{
    bitcoin::BlockHash,
    json::vrsc::{Address, Amount},
};

use crate::{PayoutMember, Stake, StakeMember};

#[allow(unused)]
#[derive(Debug)]
pub struct Payout {
    pub currencyid: Address,
    pub blockhash: BlockHash,
    pub total_work: Decimal,
    pub amount: Amount,
    // the difference between `amount` and the rewards for every payoutmember results in the pool fee:
    pub pool_fee_amount: Amount,
    pub amount_paid_to_subs: Amount,
    pub members: Vec<PayoutMember>,
}

impl Payout {
    #[instrument]
    #[allow(unused)]
    pub fn new(
        stake: &Stake,
        pool_fee_discount: Decimal,
        stake_members: Vec<StakeMember>,
        pool_identity_address: Address,
    ) -> Result<Self, Report> {
        let mut amount = stake.amount;

        let mut pool_fee_amount = Amount::ZERO;

        // sum could get pretty large, so we need to work with a bigger number:
        let shares_sum: Decimal = stake_members
            .iter()
            .fold(Decimal::ZERO, |acc, member| acc + member.shares);

        debug!("shares_sum: {}", shares_sum);

        let amount_decimal = Decimal::from_f64(amount.as_vrsc()).unwrap();

        let mut payout_members = vec![];

        for member in stake_members {
            if member.shares == Decimal::ZERO {
                warn!("a participant with 0 work was included");
                debug!("{member:?}");
                continue;
            }

            let member_fee = member
                .fee
                .checked_sub(pool_fee_discount)
                .unwrap_or(Decimal::ZERO);

            let fraction = shares_sum / member.shares;
            debug!("fraction: {fraction}");

            let shared_amount = amount_decimal.checked_div(fraction).unwrap();
            debug!("shared_amount: {shared_amount:?}");
            let mut fee_amount = shared_amount
                .checked_mul(member_fee)
                .unwrap_or(Decimal::ZERO);
            debug!("fee_amount: {fee_amount:?}");
            let shared_amount_minus_fee = shared_amount
                .checked_sub(fee_amount)
                .unwrap_or(shared_amount);
            debug!("shared_amount_minus_fee: {shared_amount_minus_fee:?}");

            let mut shared_amount_minus_fee = shared_amount_minus_fee
                .round_dp_with_strategy(8, rust_decimal::RoundingStrategy::MidpointAwayFromZero);

            shared_amount_minus_fee.rescale(8);
            fee_amount.rescale(8);

            payout_members.push(PayoutMember {
                blockhash: stake.blockhash,
                identityaddress: member.identity_address,
                reward: Amount::from_vrsc(shared_amount_minus_fee.to_f64().unwrap())?,
                shares: member.shares,
                fee: Amount::from_vrsc(fee_amount.to_f64().unwrap())?,
                txid: None,
            });
        }

        let final_calc_sum = payout_members
            .iter()
            .fold(Amount::ZERO, |acc, member| acc + member.reward);

        debug!("{final_calc_sum:#?}");
        debug!("{}", amount.as_sat());

        if let Some(pool_fee) = amount.checked_sub(final_calc_sum) {
            if pool_fee > Amount::ZERO {
                trace!(
                    "pool_fee: there is a difference of {pool_fee} between the staked amount and the amount to pay out"
                );

                pool_fee_amount += pool_fee;
            }
        }

        Ok(Self {
            currencyid: stake.currencyid.clone(),
            blockhash: stake.blockhash.clone(),
            total_work: shares_sum,
            amount: stake.amount,
            pool_fee_amount,
            amount_paid_to_subs: final_calc_sum,
            members: payout_members,
        })
    }
}

impl ToString for Payout {
    fn to_string(&self) -> String {
        let mut vec = self
            .members
            .iter()
            .map(|member| format!("{}:{:.8}", member.identityaddress, member.reward))
            .collect::<Vec<String>>();

        vec.sort_by(|a, b| b.cmp(a));

        vec.join(", ")
    }
}

#[derive(Debug, Clone)]
pub enum PayoutError {
    PayoutTooLow,
}

impl std::error::Error for PayoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }

    fn cause(&self) -> Option<&dyn std::error::Error> {
        self.source()
    }
}

impl Display for PayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PayoutTooLow => {
                f.write_str("The amount to pay out was lower than the transaction fee")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use std::str::FromStr;
    use tracing_test::traced_test;

    use crate::database;

    use super::*;

    const _VRSC: &str = "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV";
    const _VRSCTEST: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
    const ALICE: &str = "iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU";
    const BOB: &str = "iGLN3bFv6uY2HAgQgVwiGriTRgQmTyJrwi";
    const CHARLIE: &str = "RDebEHgiTFDRDUN5Uisx7ntUuRdRJHt6SK";
    // const DIRK: &str = "RSTWA7QcQaEbhS4iJha2p1b5eYvUPpVXGP";
    // const EMILY: &str = "RRVdSds5Zck6YnhYgchL8qCKqARhob64vk";
    const POOL_ADDRESS: &str = "iBnKXQnD1BFyvE8V4UVr4UKQz8h7FqfVu9";

    #[sqlx::test(fixtures("stakes"), migrator = "crate::MIGRATOR")]
    #[traced_test]
    fn single_member(pool: PgPool) -> sqlx::Result<()> {
        let stake = database::get_stake(&pool, "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV", 513251)
            .await
            .unwrap()
            .unwrap();

        let mut stake_members = vec![];
        stake_members.push(StakeMember {
            identity_address: Address::from_str(ALICE).unwrap(),
            shares: Decimal::from_f64(123.456).unwrap(),
            fee: Decimal::from_f32(0.01).unwrap(),
        });

        let payout = Payout::new(
            &stake,
            Decimal::ZERO,
            stake_members,
            Address::from_str(POOL_ADDRESS).unwrap(),
        )
        .unwrap();

        let mut to_test_against = vec![];

        to_test_against.push(PayoutMember {
            blockhash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
            )
            .unwrap(),

            identityaddress: Address::from_str(ALICE).unwrap(),
            reward: Amount::from_sat(594_099_000),
            shares: Decimal::from_f64(123.456).unwrap(),
            fee: Amount::from_sat(6_001_000),
            txid: None,
        });

        assert_eq!(payout.members, to_test_against);

        Ok(())
    }

    #[sqlx::test(fixtures("stakes"), migrator = "crate::MIGRATOR")]
    #[traced_test]
    fn double_member(pool: PgPool) -> sqlx::Result<()> {
        let stake = database::get_stake(&pool, "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV", 513251)
            .await
            .unwrap()
            .unwrap();

        let mut stake_members = vec![];
        stake_members.push(StakeMember {
            identity_address: Address::from_str(ALICE).unwrap(),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Decimal::from_f32(0.01).unwrap(),
        });

        stake_members.push(StakeMember {
            identity_address: Address::from_str(BOB).unwrap(),
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
            blockhash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
            )
            .unwrap(),

            identityaddress: Address::from_str(ALICE).unwrap(),
            reward: Amount::from_sat(297_049_500),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Amount::from_sat(3_000_500),
            txid: None,
        };

        assert!(payout.members.contains(&alice));

        let bob = PayoutMember {
            blockhash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
            )
            .unwrap(),

            identityaddress: Address::from_str(ALICE).unwrap(),
            reward: Amount::from_sat(297_049_500),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Amount::from_sat(3_000_500),
            txid: None,
        };

        assert!(payout.members.contains(&bob));

        Ok(())
    }

    #[sqlx::test(fixtures("stakes"), migrator = "crate::MIGRATOR")]
    #[traced_test]
    fn triple_member(pool: PgPool) -> sqlx::Result<()> {
        let stake = database::get_stake(&pool, "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV", 513251)
            .await
            .unwrap()
            .unwrap();

        let mut stake_members = vec![];
        stake_members.push(StakeMember {
            identity_address: Address::from_str(ALICE).unwrap(),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Decimal::from_f32(0.01).unwrap(),
        });

        stake_members.push(StakeMember {
            identity_address: Address::from_str(BOB).unwrap(),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Decimal::from_f32(0.01).unwrap(),
        });

        stake_members.push(StakeMember {
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
            blockhash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
            )
            .unwrap(),

            identityaddress: Address::from_str(ALICE).unwrap(),
            reward: Amount::from_sat(198_033_000),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Amount::from_sat(2_000_333),
            txid: None,
        };

        assert!(payout.members.contains(&alice));

        let bob = PayoutMember {
            blockhash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
            )
            .unwrap(),

            identityaddress: Address::from_str(ALICE).unwrap(),
            reward: Amount::from_sat(198_033_000),
            shares: Decimal::from_f64(5.0).unwrap(),
            fee: Amount::from_sat(2_000_333),
            txid: None,
        };

        assert!(payout.members.contains(&bob));

        let charlie = PayoutMember {
            blockhash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0",
            )
            .unwrap(),

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
        stake_members.push(StakeMember {
            identity_address: Address::from_str(ALICE).unwrap(),
            shares: Decimal::from_i64(118058969151204).unwrap(),
            fee: Decimal::from_f32(0.05).unwrap(),
        });

        stake_members.push(StakeMember {
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
            blockhash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e1",
            )
            .unwrap(),

            identityaddress: Address::from_str(ALICE).unwrap(),
            reward: Amount::from_sat(2_500_831_271),
            shares: Decimal::from_i64(118_058_969_151_204).unwrap(),
            fee: Amount::from_sat(131_622_698),
            txid: None,
        };

        assert!(payout.members.contains(&alice));

        let bob = PayoutMember {
            blockhash: BlockHash::from_str(
                "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e1",
            )
            .unwrap(),

            identityaddress: Address::from_str(BOB).unwrap(),
            reward: Amount::from_sat(4_795_168_729),
            shares: Decimal::from_i64(226_369_800_917_454).unwrap(),
            fee: Amount::from_sat(252_377_302),
            txid: None,
        };

        assert!(payout.members.contains(&alice));
        assert!(payout.members.contains(&bob));

        assert_eq!(payout.pool_fee_amount, Amount::from_sat(384_000_000));

        Ok(())
    }
}
/*
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
