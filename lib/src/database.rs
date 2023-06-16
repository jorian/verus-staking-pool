use crate::{payout::Payout, PayoutMember, Stake, StakeMember, StakeResult, Subscriber};
use color_eyre::Report;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use sqlx::{
    postgres::PgRow,
    types::{
        chrono::{DateTime, Utc},
        Decimal,
    },
    PgPool, Postgres, QueryBuilder, Row,
};
use std::{collections::HashMap, str::FromStr};
use tracing::debug;
use vrsc_rpc::{
    bitcoin::{BlockHash, Txid},
    json::vrsc::{Address, Amount},
};

#[allow(unused)]
pub async fn insert_subscriber(
    pool: &PgPool,
    currencyid: &str,
    identity_address: &str,
    identity_name: &str,
    status: &str,
    bot_address: &str,
    fee: f32,
    min_payout: u64,
) -> Result<Subscriber, Report> {
    let fee = Decimal::from_f32(fee).unwrap_or(Decimal::ZERO);

    let row = sqlx::query!(
        "INSERT INTO subscriptions(
            currencyid, identityaddress, identityname, status, pool_address, fee, min_payout
        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (currencyid, identityaddress) DO
        UPDATE SET status = $4
        WHERE subscriptions.identityaddress = EXCLUDED.identityaddress AND subscriptions.status = 'unsubscribed'
        RETURNING *",
        currencyid,
        identity_address,
        identity_name,
        status,
        bot_address,
        fee,
        min_payout as i64
    )
    .fetch_one(pool)
    .await?;

    Ok(Subscriber {
        currencyid: Address::from_str(&row.currencyid)?,
        identity_address: Address::from_str(&row.identityaddress)?,
        identity_name: row.identityname,
        pool_address: Address::from_str(&row.pool_address)?,
        status: row.status,
        min_payout: Amount::from_sat(row.min_payout as u64),
    })
}

pub async fn get_subscriber(
    pool: &PgPool,
    currencyid: &str,
    identityaddress: &str,
) -> Result<Option<Subscriber>, Report> {
    if let Some(row) = sqlx::query!(
        "SELECT * FROM subscriptions WHERE currencyid = $1 AND identityaddress = $2", // this is the primary key
        currencyid,
        identityaddress,
    )
    .fetch_optional(pool)
    .await?
    {
        return Ok(Some(Subscriber {
            currencyid: Address::from_str(&row.currencyid)?,
            identity_address: Address::from_str(&row.identityaddress)?,
            identity_name: row.identityname,
            pool_address: Address::from_str(&row.pool_address)?,
            status: row.status,
            min_payout: Amount::from_sat(row.min_payout as u64),
        }));
    }

    Ok(None)
}

// we cannot use multiple currencyids because it would mix up currencyids with identityaddresses that don't necessarily belong to the same entity.
pub async fn get_subscribers(
    pool: &PgPool,
    currencyid: &str,
    identityaddresses: &[String],
) -> Result<Vec<Subscriber>, Report> {
    sqlx::query!(
        "SELECT * 
        FROM subscriptions 
        WHERE currencyid = $1 
        AND identityaddress IN (SELECT * FROM UNNEST($2::text[]))", // this is the primary key
        currencyid,
        identityaddresses,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        Ok(Subscriber {
            currencyid: Address::from_str(&row.currencyid)?,
            identity_address: Address::from_str(&row.identityaddress)?,
            identity_name: row.identityname,
            pool_address: Address::from_str(&row.pool_address)?,
            min_payout: Amount::from_sat(row.min_payout as u64),
            status: row.status,
        })
    })
    .collect::<Result<Vec<_>, _>>()
}

pub async fn get_subscribers_by_status(
    pool: &PgPool,
    currencyid: &str,
    status_filter: &str,
) -> Result<Vec<Subscriber>, Report> {
    let rows = sqlx::query!(
        "SELECT * FROM subscriptions WHERE currencyid = $1 AND status = $2",
        currencyid,
        status_filter
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(Subscriber {
                currencyid: Address::from_str(&row.currencyid)?,
                identity_address: Address::from_str(&row.identityaddress)?,
                identity_name: row.identityname,
                pool_address: Address::from_str(&row.pool_address)?,
                min_payout: Amount::from_sat(row.min_payout as u64),
                status: row.status,
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

pub async fn update_subscriber_status(
    pool: &PgPool,
    currencyid: &str,
    identity_address: &str,
    status: &str,
) -> Result<Subscriber, Report> {
    sqlx::query!(
        "UPDATE subscriptions 
        SET status = $3
        WHERE currencyid = $1 AND identityaddress = $2
        RETURNING *",
        currencyid,
        identity_address,
        status
    )
    .fetch_one(pool)
    .await
    .map(|row| Subscriber {
        currencyid: Address::from_str(&row.currencyid).unwrap(),
        identity_address: Address::from_str(&row.identityaddress).unwrap(),
        identity_name: row.identityname.clone(),
        pool_address: Address::from_str(&row.pool_address).unwrap(),
        min_payout: Amount::from_sat(row.min_payout as u64),
        status: row.status,
    })
    .map_err(|e| e.into())
}

pub async fn update_subscriber_min_payout(
    pool: &PgPool,
    currencyid: &str,
    identity_address: &str,
    threshold: u64,
) -> Result<(), Report> {
    sqlx::query!(
        "UPDATE subscriptions 
        SET min_payout = $3
        WHERE currencyid = $1 AND identityaddress = $2",
        currencyid,
        identity_address,
        threshold as i64
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_subscriptions(
    pool: &PgPool,
    tuples: &[(String, String)],
) -> Result<Vec<Subscriber>, Report> {
    let mut query_builder: QueryBuilder<Postgres> = sqlx::QueryBuilder::new(
        "SELECT *
        FROM subscriptions 
        WHERE (currencyid, identityaddress) IN ",
    );

    query_builder.push_tuples(tuples, |mut b, tuple| {
        b.push_bind(&tuple.0).push_bind(&tuple.1);
    });

    let query = query_builder.build();
    let rows: Vec<PgRow> = query.fetch_all(pool).await?;

    let subs = rows
        .into_iter()
        .map(|row| Subscriber {
            currencyid: Address::from_str(row.get("currencyid")).unwrap(),
            identity_address: Address::from_str(row.get("identityaddress")).unwrap(),
            identity_name: row.get("identityname"),
            pool_address: Address::from_str(row.get("pool_address")).unwrap(),
            min_payout: Amount::from_sat(row.get::<i64, &str>("min_payout") as u64),
            status: row.get("status"),
        })
        .collect::<Vec<_>>();

    Ok(subs)
}

// pub async fn get_total_paid_out_by_user_id(
//     pool: &PgPool,
//     user_id: u64,
//     currencyid: &str,
// ) -> Result<sqlx::types::Decimal, Report> {
//     let row = sqlx::query!(
//         "WITH identities AS (
//             SELECT identityaddress
//             FROM subscriptions
//             WHERE discord_user_id = $1
//             AND currencyid = $2
//         )
//         SELECT SUM(amount) sum
//         FROM identities
//         INNER JOIN transactions USING (identityaddress)
//         GROUP BY identityaddress",
//         user_id as i64,
//         currencyid
//     )
//     .fetch_one(pool)
//     .await?;

//     Ok(row.sum.unwrap_or(Decimal::default()))
// }

pub async fn insert_stake(pool: &PgPool, stake: &Stake) -> Result<(), Report> {
    sqlx::query!(
        "INSERT INTO stakes(currencyid, blockhash, amount, mined_by, pos_source_txid, pos_source_vout_num, pos_source_amount, blockheight, status)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        &stake.currencyid.to_string(),
        &stake.blockhash.to_string(),
        stake.amount.as_sat() as i64,
        &stake.mined_by.to_string(),
        &stake.pos_source_txid.to_string(),
        stake.pos_source_vout_num as i16,
        stake.pos_source_amount.as_sat() as i64,
        stake.blockheight as i64,
        "pending"
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_stake(
    pool: &PgPool,
    currencyid: &str,
    blockheight: i64,
) -> Result<Option<Stake>, Report> {
    let opt_row = sqlx::query!(
        "SELECT * 
        FROM stakes
        WHERE currencyid = $1 AND blockheight = $2",
        currencyid,
        blockheight
    )
    .fetch_optional(pool)
    .await?;

    if let Some(row) = opt_row {
        return Ok(Some(Stake::new(
            Address::from_str(&row.currencyid).unwrap(),
            BlockHash::from_str(&row.blockhash).unwrap(),
            Address::from_str(&row.mined_by).unwrap(),
            Txid::from_str(&row.pos_source_txid).unwrap(),
            row.pos_source_vout_num as u16,
            Amount::from_sat(row.pos_source_amount.try_into().unwrap()),
            StakeResult::from_str(&row.result.unwrap_or(String::new())).unwrap(),
            Amount::from_sat(row.amount.try_into().unwrap()),
            row.blockheight as u64,
        )));
    } else {
        return Ok(None);
    }
}

pub async fn set_stake_result(pool: &PgPool, stake: &Stake) -> Result<(), Report> {
    sqlx::query!(
        "UPDATE stakes SET status = 'processed', result = $1 WHERE currencyid = $2 AND blockhash = $3",
        &stake.result.to_string(),
        &stake.currencyid.to_string(),
        &stake.blockhash.to_string(),
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_pending_stakes(pool: &PgPool, currencyid: &str) -> Result<Vec<Stake>, Report> {
    let rows = sqlx::query!(
        "SELECT * FROM stakes WHERE currencyid = $1 AND status = 'pending'",
        currencyid
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            Stake::new(
                Address::from_str(&row.currencyid).unwrap(),
                BlockHash::from_str(&row.blockhash).unwrap(),
                Address::from_str(&row.mined_by).unwrap(),
                Txid::from_str(&row.pos_source_txid).unwrap(),
                row.pos_source_vout_num as u16,
                Amount::from_sat(row.pos_source_amount.try_into().unwrap()),
                StakeResult::from_str(&row.result.unwrap_or(String::new())).unwrap(),
                Amount::from_sat(row.amount.try_into().unwrap()),
                row.blockheight as u64,
            )
        })
        .collect::<Vec<_>>())
}

pub async fn get_recent_stakes(
    pool: &PgPool,
    currencyid: &str,
    since: DateTime<Utc>,
) -> Result<Vec<Stake>, Report> {
    let rows = sqlx::query!(
        "SELECT * 
        FROM stakes 
        WHERE currencyid = $1 AND created_at > $2",
        currencyid,
        since
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            Stake::new(
                Address::from_str(&row.currencyid).unwrap(),
                BlockHash::from_str(&row.blockhash).unwrap(),
                Address::from_str(&row.mined_by).unwrap(),
                Txid::from_str(&row.pos_source_txid).unwrap(),
                row.pos_source_vout_num as u16,
                Amount::from_sat(row.pos_source_amount.try_into().unwrap()),
                StakeResult::from_str(&row.result.unwrap_or(String::new())).unwrap(),
                Amount::from_sat(row.amount.try_into().unwrap()),
                row.blockheight as u64,
            )
        })
        .collect::<Vec<_>>())
}

pub async fn get_latest_round(pool: &PgPool, currencyid: &str) -> Result<Option<i64>, Report> {
    if let Some(row) = sqlx::query!(
        "SELECT latest_round FROM latest_state WHERE currencyid = $1",
        currencyid
    )
    .fetch_optional(pool)
    .await?
    {
        return Ok(Some(row.latest_round));
    }

    Ok(None)
}

pub async fn upsert_work(
    pool: &PgPool,
    currencyid: &str,
    payload: &HashMap<Address, Decimal>,
    latest_blockheight: u64,
) -> Result<(), Report> {
    let mut tx = pool.begin().await?;

    let mut query_builder: QueryBuilder<Postgres> =
        QueryBuilder::new("INSERT INTO work (currencyid, round, address, shares) ");

    let tuples = payload
        .iter()
        .map(|(address, shares)| (currencyid, 0, address.to_string(), shares));

    debug!("tuples: {tuples:?}");

    query_builder.push_values(tuples, |mut b, tuple| {
        b.push_bind(tuple.0)
            .push_bind(tuple.1)
            .push_bind(tuple.2)
            .push_bind(tuple.3);
    });

    query_builder.push(
        " ON CONFLICT (currencyid, round, address) 
        DO UPDATE SET shares = work.shares + EXCLUDED.shares
        WHERE work.currencyid = EXCLUDED.currencyid 
        AND work.round = EXCLUDED.round 
        AND work.address = EXCLUDED.address",
    );

    query_builder.build().execute(&mut tx).await?;

    let mut query_builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "INSERT INTO latest_state (currencyid, address, latest_round, latest_work) ",
    );

    let tuples = payload.iter().map(|(address, shares)| {
        (
            currencyid,
            address.to_string(),
            latest_blockheight as i64,
            shares,
        )
    });

    debug!("tuples latest_state: {tuples:?}");

    query_builder.push_values(tuples, |mut b, tuple| {
        b.push_bind(tuple.0)
            .push_bind(tuple.1)
            .push_bind(tuple.2)
            .push_bind(tuple.3);
    });

    query_builder.push(
        " ON CONFLICT (currencyid, address) 
        DO UPDATE SET latest_round = EXCLUDED.latest_round, latest_work = EXCLUDED.latest_work
        WHERE latest_state.currencyid = EXCLUDED.currencyid 
        AND latest_state.address = EXCLUDED.address",
    );

    query_builder.build().execute(&mut tx).await?;

    tx.commit().await?;

    Ok(())
}

pub async fn move_work_to_round(
    pool: &PgPool,
    currencyid: &str,
    from_round: i64,
    to_round: i64,
) -> Result<(), Report> {
    sqlx::query!(
        "UPDATE work SET round = $3 WHERE currencyid = $1 AND round = $2",
        currencyid,
        from_round,
        to_round
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn move_work_to_current_round(
    pool: &PgPool,
    currencyid: &str,
    from_round: u64,
) -> Result<(), Report> {
    sqlx::query!(
        "WITH round_to_move AS (
            SELECT currencyid, round, address, shares
            FROM work 
            WHERE currencyid = $1 AND round = $2
        )
        INSERT INTO work (currencyid, round, address, shares) 
        SELECT currencyid, 0, address, shares
        FROM round_to_move
        ON CONFLICT (currencyid, round, address)
        DO UPDATE SET shares = work.shares + EXCLUDED.shares",
        currencyid,
        from_round as i64
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_work_and_fee_by_round(
    pool: &PgPool,
    currencyid: &str,
    round: u64,
) -> Result<Vec<StakeMember>, Report> {
    let rows = sqlx::query!(
        "SELECT address, shares, fee FROM subscriptions s1
        JOIN work w1
        ON w1.address = s1.identityaddress AND s1.currencyid = w1.currencyid
        WHERE w1.round = $1 AND w1.currencyid = $2",
        round as i64,
        currencyid
    )
    .fetch_all(pool)
    .await?;

    debug!("{:#?}", &rows);

    Ok(rows
        .iter()
        .map(|row| StakeMember {
            identity_address: Address::from_str(&row.address).unwrap(),
            shares: row.shares.unwrap_or(Decimal::ZERO),
            fee: row.fee,
        })
        .collect::<Vec<_>>())
}

pub async fn get_total_paid_out_by_identity_address(
    pool: &PgPool,
    identity_address: &str,
    currencyid: &str,
) -> Result<Vec<(String, i64)>, Report> {
    let rows = sqlx::query!(
        // "SELECT identityaddress, SUM(amount) as sum
        // FROM transactions
        // WHERE identityaddress IN (SELECT * FROM UNNEST($1::text[]))
        //     AND currencyid = $2
        // GROUP BY identityaddress",
        "SELECT identityaddress, SUM(reward) as sum
        FROM payout_members 
        WHERE identityaddress = $1
            AND currencyid = $2
        GROUP BY identityaddress",
        identity_address,
        currencyid
    )
    .fetch_all(pool)
    .await?;

    let v = rows
        .into_iter()
        .map(|r| (r.identityaddress, r.sum.unwrap().to_i64().unwrap()))
        .collect::<Vec<(String, i64)>>();

    Ok(v)
}

pub async fn insert_transaction(
    pool: &PgPool,
    currencyid: &str,
    txid: &str,
    identity_address: &str,
    amount: u64,
    shares: sqlx::types::Decimal,
) -> Result<(), Report> {
    sqlx::query!(
        "
        INSERT INTO transactions(currencyid, txid, identityaddress, amount, shares) 
        VALUES ($1, $2, $3, $4, $5)
    ",
        currencyid,
        txid,
        identity_address,
        amount as i64,
        &shares
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_transaction_ids(pool: &PgPool) -> Result<Vec<String>, Report> {
    Ok(sqlx::query!("SELECT txid FROM transactions")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| row.txid)
        .collect::<Vec<String>>())
}

pub async fn get_latest_state_for_subscribers(
    pool: &PgPool,
    currencyid: &str,
) -> Result<HashMap<Address, Decimal>, Report> {
    let rows = sqlx::query!(
        "SELECT identityaddress, latest_work
        FROM subscriptions
        INNER JOIN latest_state ON identityaddress = address
        WHERE latest_state.currencyid = $1",
        currencyid
    )
    .fetch_all(pool)
    .await?;

    let map = rows
        .into_iter()
        .map(|row| {
            (
                Address::from_str(&row.identityaddress).unwrap(),
                row.latest_work,
            )
        })
        .collect::<HashMap<Address, Decimal>>();

    Ok(map)
}

pub async fn insert_payout(pool: &PgPool, payout: &Payout) -> Result<(), Report> {
    sqlx::query!(
        "INSERT INTO payouts(
            currencyid, blockhash, blockheight, amount, totalwork, pool_fee_amount, amount_paid_to_subs, n_subs
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &payout.currencyid.to_string(),
        &payout.blockhash.to_string(),
        payout.blockheight as i64,
        payout.amount.as_sat() as i64,
        &payout.total_work,
        payout.pool_fee_amount.as_sat() as i64,
        payout.amount_paid_to_subs.as_sat() as i64,
        payout.members.len() as i64
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert_payout_members(pool: &PgPool, payout: &Payout) -> Result<(), Report> {
    let currencyid = payout.currencyid.to_string();
    let blockhash = payout.blockhash.to_string();
    let blockheight = payout.blockheight as i64;
    let mut tx = pool.begin().await?;

    let mut query_builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "INSERT INTO payout_members (currencyid, blockhash, blockheight, identityaddress, shares, reward, fee) ",
    );

    let tuples = payout.members.iter().map(|member| {
        (
            &currencyid,
            &blockhash,
            blockheight,
            member.identityaddress.to_string(),
            member.shares,
            member.reward.as_sat() as i64,
            member.fee.as_sat() as i64,
        )
    });

    query_builder.push_values(tuples, |mut b, tuple| {
        b.push_bind(tuple.0)
            .push_bind(tuple.1)
            .push_bind(tuple.2)
            .push_bind(tuple.3)
            .push_bind(tuple.4)
            .push_bind(tuple.5)
            .push_bind(tuple.6);
    });

    query_builder.build().execute(&mut tx).await?;
    tx.commit().await?;

    Ok(())
}

pub async fn get_payout_members_without_payment(
    pool: &PgPool,
    currencyid: &str,
) -> Result<Option<Vec<PayoutMember>>, Report> {
    let rows = sqlx::query!(
        "SELECT * FROM payout_members WHERE currencyid = $1 AND payment_txid is null",
        currencyid
    )
    .fetch_all(pool)
    .await?;

    debug!("{:?}", rows);

    let payout_members = rows
        .iter()
        .map(|row| PayoutMember {
            blockhash: BlockHash::from_str(&row.blockhash).unwrap(),
            blockheight: row.blockheight as u64,
            identityaddress: Address::from_str(&row.identityaddress).unwrap(),
            reward: Amount::from_sat(row.reward as u64),
            shares: row.shares,
            fee: Amount::from_sat(row.fee as u64),
            txid: row
                .payment_txid
                .as_ref()
                .map(|s| Txid::from_str(&s).unwrap()),
        })
        .collect::<Vec<PayoutMember>>();

    if payout_members.is_empty() {
        Ok(None)
    } else {
        Ok(Some(payout_members))
    }
}

pub async fn update_payment_members(
    pool: &PgPool,
    currencyid: &str,
    payment_members: impl Iterator<Item = &PayoutMember>,
    txid: &str,
) -> Result<(), Report> {
    let mut tx = pool.begin().await?;

    let mut query_builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "UPDATE payout_members AS pm 
        SET payment_txid = ppm.payment_txid from ( ",
    );

    let tuples = payment_members.map(|member| {
        (
            currencyid,
            member.identityaddress.to_string(),
            member.blockhash.to_string(),
            txid,
        )
    });

    query_builder.push_values(tuples, |mut b, tuple| {
        b.push_bind(tuple.0)
            .push_bind(tuple.1)
            .push_bind(tuple.2)
            .push_bind(tuple.3);
    });

    query_builder.push(
        " ) AS ppm(currencyid, identityaddress, blockhash, payment_txid) 
    WHERE ppm.currencyid = pm.currencyid 
    AND ppm.blockhash = pm.blockhash
    AND ppm.identityaddress = pm.identityaddress",
    );

    query_builder.build().execute(&mut tx).await?;
    tx.commit().await?;

    Ok(())
}

pub async fn get_payouts(
    pool: &PgPool,
    currencyid: &str,
    identityaddresses: &[String],
) -> Result<Vec<PayoutMember>, Report> {
    sqlx::query!(
        "SELECT * 
        FROM payout_members 
        WHERE currencyid = $1 
        AND identityaddress IN (SELECT * FROM UNNEST($2::text[]))", // this is the primary key
        currencyid,
        identityaddresses,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        Ok(PayoutMember {
            blockhash: BlockHash::from_str(&row.blockhash).unwrap(),
            blockheight: row.blockheight as u64,
            identityaddress: Address::from_str(&row.identityaddress).unwrap(),
            reward: Amount::from_sat(row.reward.try_into()?),
            shares: row.shares,
            fee: Amount::from_sat(row.fee.try_into()?),
            txid: row
                .payment_txid
                .as_ref()
                .map(|s| Txid::from_str(&s).unwrap()),
        })
    })
    .collect::<Result<Vec<_>, _>>()
}

pub async fn get_pool_fees(pool: &PgPool, currencyid: &str) -> Result<Amount, Report> {
    let row = sqlx::query!(
        "SELECT SUM (pool_fee_amount) AS total
        FROM payouts
        WHERE currencyid = $1",
        currencyid
    )
    .fetch_one(pool)
    .await?;

    let dec = row.total.unwrap_or(Decimal::ZERO);
    Ok(Amount::from_sat(dec.to_i64().unwrap() as u64))
}

#[cfg(test)]
mod tests {
    use std::ops::SubAssign;

    use rust_decimal::prelude::FromPrimitive;
    use sqlx::{types::chrono, Row};

    use super::*;
    use tracing_test::traced_test;

    // also used in /fixtures
    const VRSC: &str = "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV";
    const ALICE: &str = "iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU";
    const BOB: &str = "iGLN3bFv6uY2HAgQgVwiGriTRgQmTyJrwi";

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn test_insert_transaction(pool: PgPool) -> sqlx::Result<()> {
        let mut conn = pool.acquire().await?;

        let txid = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let amount = 1002;
        let shares = Decimal::from_f64(54321.12345678).unwrap();

        insert_transaction(&pool, VRSC, txid, ALICE, amount, shares)
            .await
            .unwrap();

        let inserted_transaction = sqlx::query("SELECT * FROM transactions")
            .fetch_one(&mut conn)
            .await?;

        assert!(inserted_transaction.get::<String, &str>("identityaddress") == ALICE.to_string());

        assert!(inserted_transaction.get::<i64, &str>("amount") == amount as i64);
        assert!(inserted_transaction.get::<Decimal, &str>("shares") == shares);

        Ok(())
    }

    #[traced_test]
    #[sqlx::test(fixtures("subscriptions"), migrator = "crate::MIGRATOR")]
    async fn test_get_subscriptions(pool: PgPool) -> sqlx::Result<()> {
        let subscriptions = get_subscriptions(
            &pool,
            &[(
                "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV".to_string(),
                "iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU".to_string(),
            )],
        )
        .await
        .unwrap();

        debug!("{:?}", subscriptions);
        assert!(subscriptions.len() == 1);

        Ok(())
    }

    #[sqlx::test(fixtures("transactions"), migrator = "crate::MIGRATOR")]
    async fn test_get_transaction_ids(pool: PgPool) -> sqlx::Result<()> {
        let txns = get_transaction_ids(&pool).await.unwrap();

        assert!(txns
            .iter()
            .any(|tx| &*tx == "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));

        Ok(())
    }

    #[sqlx::test(fixtures("transactions"), migrator = "crate::MIGRATOR")]
    async fn test_get_summed_amount_by_identity(pool: PgPool) -> sqlx::Result<()> {
        let sums = get_total_paid_out_by_identity_address(&pool, &ALICE.to_owned(), VRSC)
            .await
            .unwrap();

        assert!(sums
            .iter()
            .any(|sum| &sum.0 == ALICE && sum.1 == 600_000_000));

        Ok(())
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn insert_pending_subscriber(pool: PgPool) -> sqlx::Result<()> {
        let mut conn = pool.acquire().await?;

        let identity_name = "test@";
        let status = "pending";
        let bot_address = "RKsy4WEdAd29XAvTJsKaDNtD3PxGRzSQdu";

        insert_subscriber(
            &pool,
            VRSC,
            ALICE,
            identity_name,
            status,
            bot_address,
            5.0,
            1_000_000_000,
        )
        .await
        .unwrap();

        let inserted_subscriber = sqlx::query("SELECT * FROM subscriptions")
            .fetch_one(&mut conn)
            .await?;

        assert_eq!(
            ALICE.to_string(),
            inserted_subscriber.get::<String, &str>("identityaddress")
        );
        assert_eq!(
            Decimal::from_f32(5.0).unwrap(),
            inserted_subscriber.get::<Decimal, &str>("fee")
        );

        Ok(())
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn upsert(pool: PgPool) -> sqlx::Result<()> {
        let mut conn = pool.acquire().await?;

        let mut payload = HashMap::new();

        payload.insert(
            Address::from_str(ALICE).unwrap(),
            Decimal::from_u64(50).unwrap(),
        );

        upsert_work(&pool, VRSC, &payload, 1).await.unwrap();

        payload.insert(
            Address::from_str(BOB).unwrap(),
            Decimal::from_u64(5000).unwrap(),
        );

        upsert_work(&pool, VRSC, &payload, 1).await.unwrap();
        upsert_work(&pool, VRSC, &payload, 1).await.unwrap();

        let work = sqlx::query("SELECT * FROM work")
            .fetch_all(&mut conn)
            .await?;

        assert!(work.len() == 2);
        assert!(work
            .iter()
            .any(|row| { row.get::<Decimal, &str>("shares") == Decimal::from_u64(150).unwrap() }));
        assert!(work.iter().any(|row| {
            row.get::<Decimal, &str>("shares") == Decimal::from_u64(10000).unwrap()
        }));

        Ok(())
    }

    #[sqlx::test(fixtures("work"), migrator = "crate::MIGRATOR")]
    async fn test_move_work_to_current_round(pool: PgPool) -> sqlx::Result<()> {
        let mut conn = pool.acquire().await?;

        move_work_to_current_round(&pool, VRSC, 1).await.unwrap();

        let work = sqlx::query("SELECT * FROM work")
            .fetch_all(&mut conn)
            .await?;

        assert!(work
            .iter()
            .any(|row| { row.get::<Decimal, &str>("shares") == Decimal::from_u64(550).unwrap() }));

        assert!(work
            .iter()
            .any(|row| { row.get::<Decimal, &str>("shares") == Decimal::from_u64(110).unwrap() }));

        Ok(())
    }

    #[sqlx::test(
        fixtures("latest_state", "subscriptions"),
        migrator = "crate::MIGRATOR"
    )]
    async fn test_get_latest_state_for_subscribers(pool: PgPool) -> sqlx::Result<()> {
        let latest_state = get_latest_state_for_subscribers(&pool, VRSC).await.unwrap();

        assert!(latest_state
            .get(&Address::from_str(ALICE).unwrap())
            .is_some());

        assert_eq!(
            latest_state
                .get(&Address::from_str(ALICE).unwrap())
                .unwrap(),
            &Decimal::from_i64(100000000000).unwrap()
        );

        assert_ne!(
            latest_state
                .get(&Address::from_str(ALICE).unwrap())
                .unwrap(),
            &Decimal::from_i64(100000000001).unwrap()
        );

        Ok(())
    }

    #[traced_test]
    #[sqlx::test(fixtures("work", "subscriptions"), migrator = "crate::MIGRATOR")]
    async fn test_get_work_exported_id(pool: PgPool) -> sqlx::Result<()> {
        let work = get_work_and_fee_by_round(&pool, VRSC, 1).await.unwrap();

        assert!(work.len() == 2);
        assert!(work
            .iter()
            .find(|member| &member.identity_address.to_string()
                == "iGLN3bFv6uY2HAgQgVwiGriTRgQmTyJrwi")
            .is_some());

        assert!(work
            .iter()
            .find(|member| &member.identity_address.to_string()
                == "iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU")
            .is_some());

        Ok(())
    }

    #[traced_test]
    #[sqlx::test(fixtures("payout_members"), migrator = "crate::MIGRATOR")]
    async fn test_eligible_for_payout(pool: PgPool) -> sqlx::Result<()> {
        get_payout_members_without_payment(&pool, "iAe2MD8b47o9qfBYPwF4pM8jQp2kEBpUkH")
            .await
            .unwrap();

        Ok(())
    }

    #[traced_test]
    #[sqlx::test(fixtures("stakes"), migrator = "crate::MIGRATOR")]
    async fn test_recent_stakes(pool: PgPool) -> sqlx::Result<()> {
        let mut since = chrono::Utc::now();
        let duration = ::chrono::Duration::days(3);
        since.sub_assign(duration);
        debug!("{:?}", since);

        let stakes = get_recent_stakes(&pool, "i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV", since)
            .await
            .unwrap();

        assert_eq!(stakes.len(), 1);

        Ok(())
    }
}
