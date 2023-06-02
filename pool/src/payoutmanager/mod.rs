use std::{collections::HashMap, time::Duration};

use color_eyre::Report;
// TODO: this should not be the case, it should be encapsulated and only approachable through payoutmanager
use poollib::{database, payout::Payout, PayoutMember, PgPool, Stake};
use rust_decimal::Decimal;
use tracing::{debug, error, trace};
use vrsc_rpc::{
    bitcoin::Txid,
    json::vrsc::{Address, Amount},
    Client, RpcApi, SendCurrencyOutput,
};

#[derive(Debug)]
pub enum PayoutManagerError {
    DbWriteFail,
}

impl std::error::Error for PayoutManagerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }

    fn cause(&self) -> Option<&dyn std::error::Error> {
        self.source()
    }
}

impl std::fmt::Display for PayoutManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DbWriteFail => write!(f, "The transaction failed"),
        }
    }
}

#[derive(Debug)]
pub struct PayoutManager {
    _pool: PgPool,
}

impl PayoutManager {
    pub async fn create_payout(
        pool: &PgPool,
        stake: &Stake,
        bot_fee_discount: Decimal,
        bot_identity_address: Address,
    ) -> Result<Payout, Report> {
        // need to gather the work that belongs to this blockheight in order to do the calculation
        let work = database::get_work_and_fee_by_round(
            &pool,
            &stake.currencyid.to_string(),
            stake.blockheight,
        )
        .await?;

        debug!("{:#?}", &work);

        let payout = Payout::new(&stake, bot_fee_discount, work, bot_identity_address);

        debug!("{:#?}", payout);

        // store payout statement in database
        // store payment members in database
        // update balances for subscribers

        payout
    }

    // stores both payout and its members in the database.
    pub async fn store_payout_in_database(pool: &PgPool, payout: &Payout) -> Result<(), Report> {
        database::insert_payout(pool, payout).await?;
        database::insert_payout_members(pool, payout).await?;

        Ok(())
    }

    // do a query that compares the balances table with the subscribers table.
    // get the balances where the balance >= the min_payout of a subscriber
    pub async fn get_eligible_for_payout(
        pool: &PgPool,
        currencyid: &str,
    ) -> Result<Option<HashMap<Address, Vec<PayoutMember>>>, Report> {
        trace!("finding payout members eligible for payment");

        if let Some(payout_members) =
            database::get_payout_members_without_payment(pool, currencyid).await?
        {
            trace!("payout members pending payment: {:#?}", &payout_members);

            let mut payout_members_map: HashMap<Address, Vec<PayoutMember>> = HashMap::new();

            for member in payout_members.into_iter() {
                payout_members_map
                    .entry(member.identityaddress.clone())
                    .and_modify(|v| v.push(member.clone()))
                    .or_insert(vec![member]);
            }

            // get them as subscribers to get min_payout settings:
            // TODO this could be done as one query, of course
            let subscribers = database::get_subscribers(
                pool,
                &currencyid,
                &payout_members_map
                    .keys()
                    .map(|address| address.to_string())
                    .collect::<Vec<String>>(),
            )
            .await?;

            debug!("{:#?}", &subscribers);

            for subscriber in subscribers {
                let min_payout = subscriber.min_payout;
                if payout_members_map
                    .get(&subscriber.identity_address)
                    .unwrap() // unwrap because we just got subscribers based on payout_members
                    .iter()
                    .fold(Amount::ZERO, |acc, sum| acc + sum.reward)
                    < min_payout
                {
                    payout_members_map.remove(&subscriber.identity_address);
                }
            }

            if payout_members_map.len() > 0 {
                return Ok(Some(payout_members_map));
            }
        }

        trace!("no payment candidates this time");

        Ok(None)
    }

    pub fn prepare_payment<'a>(
        payout_members: &HashMap<Address, Vec<PayoutMember>>,
        client: &Client,
    ) -> Result<Vec<SendCurrencyOutput<'a>>, Report> {
        let payment_vouts = payout_members
            .iter()
            .map(|(address, epm)| {
                (
                    address,
                    epm.iter().fold(Amount::ZERO, |acc, pm| acc + pm.reward),
                )
            })
            .collect::<HashMap<&Address, Amount>>();

        debug!("payment_vouts {:#?}", payment_vouts);

        let outputs = payment_vouts
            .iter()
            .map(|(address, amount)| SendCurrencyOutput::new(None, amount, &address.to_string()))
            .collect::<Vec<_>>();

        Ok(outputs)
    }

    pub async fn send_payment<'a>(
        outputs: Vec<SendCurrencyOutput<'a>>,
        pool_address: &Address,
        client: &Client,
    ) -> Result<Option<Txid>, Report> {
        let opid = client.send_currency(&pool_address.to_string(), outputs, None, None)?;

        if let Some(txid) = wait_for_sendcurrency_finish(&client, &opid).await? {
            trace!("{txid}");

            return Ok(Some(txid));
        }

        Ok(None)
    }
}

async fn wait_for_sendcurrency_finish(client: &Client, opid: &str) -> Result<Option<Txid>, Report> {
    // from https://buildmedia.readthedocs.org/media/pdf/zcash/english-docs/zcash.pdf
    // status can be one of queued, executing, failed or success.
    // we should sleep if status is one of queued or executing
    // we should return when status is one of failed or success.
    loop {
        trace!("getting operation status: {}", &opid);
        let operation_status = client.z_get_operation_status(vec![&opid])?;
        trace!("got operation status: {:?}", &operation_status);

        if let Some(Some(opstatus)) = operation_status.first() {
            if ["queued", "executing"].contains(&opstatus.status.as_ref()) {
                tokio::time::sleep(Duration::from_millis(100)).await;
                trace!("opid still executing");
                continue;
            }

            if let Some(txid) = &opstatus.result {
                trace!(
                    "there was an operation_status, operation was executed with status: {}",
                    opstatus.status
                );

                return Ok(Some(txid.txid));
            } else {
                error!("execution failed with status: {}", opstatus.status);
            }
        } else {
            trace!("there was NO operation_status");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
