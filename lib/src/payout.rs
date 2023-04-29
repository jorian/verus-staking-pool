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
    pub bot_fee_amount: Amount,
    pub amount_paid_to_subs: Amount,
    pub members: Vec<PayoutMember>,
    // pub final_calc: HashMap<Address, Amount>,
}

impl Payout {
    #[instrument]
    #[allow(unused)]
    pub fn new(
        stake: &Stake,
        bot_fee_discount: Decimal,
        stake_members: Vec<StakeMember>,
        bot_identity_address: Address,
    ) -> Result<Self, Report> {
        let mut amount = stake.amount;
        let mut bot_fee_amount = Amount::ZERO;

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
                .checked_sub(bot_fee_discount)
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
                .round_dp_with_strategy(8, rust_decimal::RoundingStrategy::ToZero);
            shared_amount_minus_fee.rescale(8);

            fee_amount.rescale(8);

            payout_members.push(PayoutMember {
                blockhash: stake.blockhash,
                identityaddress: member.identity_address,
                reward: Amount::from_vrsc(shared_amount_minus_fee.to_f64().unwrap())?,
                shares: member.shares,
                fee: Amount::from_vrsc(fee_amount.to_f64().unwrap())?,
            });
        }

        // let final_calc_sum = final_calc.values().fold(0, |acc, sum| acc + sum.as_sat());
        let final_calc_sum = payout_members
            .iter()
            .fold(Amount::ZERO, |acc, member| acc + member.reward);

        debug!("{final_calc_sum:#?}");
        debug!("{}", amount.as_sat());

        if let Some(leftovers) = amount.checked_sub(final_calc_sum) {
            if leftovers > Amount::ZERO {
                trace!(
                    "leftovers: the payout is lower, cannot divide amount evenly over participants"
                );
                debug!("leftovers: {leftovers}");

                bot_fee_amount += leftovers;
            }
        }

        Ok(Self {
            currencyid: stake.currencyid.clone(),
            blockhash: stake.blockhash.clone(),
            total_work: shares_sum,
            amount: stake.amount,
            bot_fee_amount,
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
    // FeeTooHigh,
    // FeeAbnormal,
    // FeeNegative,
    // ZeroWork,
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
            // Self::FeeAbnormal => f.write_str("Fee is not correct"),
            // Self::FeeNegative => f.write_str("Fee is negative"),
            // Self::ZeroWork => f.write_str("Participant has no shares, should not be in hashmap"),
        }
    }
}
/*
#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use tracing_test::traced_test;
    use vrsc_rpc::Auth;

    use super::*;
    fn client() -> Client {
        Client::vrsc(false, Auth::ConfigFile).expect("a client")
    }

    const _VRSC: &str = "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV";
    const _VRSCTEST: &str = "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq";
    const ALICE: &str = "iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU";
    const BOB: &str = "iGLN3bFv6uY2HAgQgVwiGriTRgQmTyJrwi";
    const CHARLIE: &str = "RDebEHgiTFDRDUN5Uisx7ntUuRdRJHt6SK";
    const DIRK: &str = "RSTWA7QcQaEbhS4iJha2p1b5eYvUPpVXGP";
    const EMILY: &str = "RRVdSds5Zck6YnhYgchL8qCKqARhob64vk";
    const BOT_ADDRESS: &str = "iBnKXQnD1BFyvE8V4UVr4UKQz8h7FqfVu9";

    #[test]
    #[traced_test]
    fn andromeda() {
        let mut members = vec![];
        members.push(PayoutMember {
            identityaddress: Address::from_str(ALICE).unwrap(),
            shares: Decimal::from_i64(405890416898688).unwrap(),
            fee: Decimal::from_f32(0.05).unwrap(), // 0.5%
        });
        members.push(PayoutMember {
            identityaddress: Address::from_str(ALICE).unwrap(),
            shares: Decimal::from_i64(405890416898688).unwrap(),
            fee: Decimal::from_f32(0.05).unwrap(), // 0.5%
        });
        members.push(PayoutMember {
            identityaddress: Address::from_str(ALICE).unwrap(),
            shares: Decimal::from_i64(405890416898688).unwrap(),
            fee: Decimal::from_f32(0.05).unwrap(), // 0.5%
        });

        let payout = Payout::new(
            Amount::from_sat(600000000),
            // Amount::ZERO,
            Decimal::ZERO,
            members,
            Address::from_str("iAetFs8T3hdePUpFVj2m5hhLfVMnVKJ8qt").unwrap(),
            // client(),
        )
        .unwrap();

        debug!("{:#?}", payout.final_calc);
    }

    #[test]
    #[traced_test]
    fn single_member() {
        let mut members = vec![];
        members.push(PayoutMember {
            identityaddress: Address::from_str(CHARLIE).unwrap(),
            shares: Decimal::from_f32(456.0).unwrap(),
            fee: Decimal::from_f32(0.005).unwrap(), // 0.5%
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
            Amount::from_sat(596_990_050),
        );

        final_calc.insert(
            Address::from_str(BOT_ADDRESS).unwrap(),
            Amount::from_sat(2_999_950),
        );

        assert_eq!(payout.final_calc, final_calc);
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
