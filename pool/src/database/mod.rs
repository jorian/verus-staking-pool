mod constants;

use std::collections::HashMap;
use std::str::FromStr;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::types::Decimal;
use sqlx::Row;
use sqlx::{postgres::PgRow, PgPool, Postgres, QueryBuilder};
use tracing::debug;
use vrsc_rpc::json::identity::{self, Identity};
use vrsc_rpc::json::vrsc::{Address, Amount};

use crate::coinstaker::constants::{Stake, Staker};
use crate::coinstaker::{CurrencyId, IdentityAddress, StakerStatus};
use crate::database::constants::DbStaker;

#[allow(unused)]
pub async fn store_staker(
    pool: &PgPool,
    currency_address: &Address,
    identity: &Identity,
    status: StakerStatus,
    min_payout: u64,
) -> Result<()> {
    debug!("storing in database.");

    sqlx::query_file!(
        "sql/store_staker.sql",
        currency_address.to_string(),
        identity.identity.identityaddress.to_string(),
        identity.fullyqualifiedname,
        status as StakerStatus,
        min_payout as i64
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_stakers_by_identity_address(
    pool: &PgPool,
    currency_address: &Address,
    identity_addresses: Vec<Address>,
) -> Result<Vec<Staker>> {
    let mut query_builder: QueryBuilder<Postgres> = sqlx::QueryBuilder::new(
        "SELECT *
        FROM stakers 
        WHERE (currencyid, identityaddress) IN ",
    );

    query_builder.push_tuples(identity_addresses, |mut b, identity_address| {
        b.push_bind(currency_address.to_string())
            .push_bind(identity_address.to_string());
    });

    let query = query_builder.build();
    let rows: Vec<PgRow> = query.fetch_all(pool).await?;

    let subs = rows
        .into_iter()
        .map(|row| Staker {
            currency_address: Address::from_str(row.get("currencyid")).unwrap(),
            identity_address: Address::from_str(row.get("identityaddress")).unwrap(),
            identity_name: row.get("identityname"),
            min_payout: Amount::from_sat(row.get::<i64, &str>("min_payout") as u64),
            status: row.get::<String, &str>("status").try_into().unwrap(),
        })
        .collect::<Vec<_>>();

    Ok(subs)
}

pub async fn get_stakers_by_status(
    pool: &PgPool,
    currency_address: &Address,
    status: StakerStatus,
) -> Result<Vec<Staker>> {
    let rows = sqlx::query_as!(
        DbStaker,
        r#"SELECT 
            currency_address, 
            identity_address, 
            identity_name, 
            min_payout, 
            status AS "status: _"
        FROM stakers 
        WHERE currency_address = $1 
            AND status = $2"#,
        currency_address.to_string(),
        status as StakerStatus
    )
    .try_map(Staker::try_from)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

pub async fn get_staker(
    pool: &PgPool,
    currency_address: &Address,
    identity_address: &Address,
) -> Result<Option<Staker>> {
    let staker = sqlx::query_as!(
        DbStaker,
        r#"SELECT 
            currency_address, 
            identity_address, 
            identity_name, 
            min_payout, 
            status AS "status: _"
        FROM stakers 
        WHERE currency_address = $1 
            AND identity_address = $2"#,
        currency_address.to_string(),
        identity_address.to_string()
    )
    .try_map(Staker::try_from)
    .fetch_optional(pool)
    .await?;

    Ok(staker)
}

/// Stores work for every staking participant in this staking round.
///
/// Every active staker gets their share (their stake) added as work.
/// Payload contains all the addresses and their stake, which are written to the database.
pub async fn store_work(
    pool: &PgPool,
    currency_address: &Address,
    payload: HashMap<Address, Decimal>,
    last_blockheight: u64,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    let mut query_builder: QueryBuilder<Postgres> =
        QueryBuilder::new("INSERT INTO work (currency_address, round, staker_address, shares) ");

    let tuples = payload.iter().map(|(staker_address, shares)| {
        (
            currency_address.to_string(),
            0,
            staker_address.to_string(),
            shares,
        )
    });

    debug!("tuples work: {tuples:?}");

    query_builder.push_values(tuples, |mut b, tuple| {
        b.push_bind(tuple.0)
            .push_bind(tuple.1)
            .push_bind(tuple.2)
            .push_bind(tuple.3);
    });

    query_builder.push(
        " ON CONFLICT (currency_address, round, staker_address) 
        DO UPDATE SET shares = work.shares + EXCLUDED.shares
        WHERE work.currency_address = EXCLUDED.currency_address 
        AND work.round = EXCLUDED.round 
        AND work.staker_address = EXCLUDED.staker_address",
    );

    query_builder.build().execute(&mut *tx).await?;

    let mut query_builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "INSERT INTO last_state (currency_address, staker_address, latest_round, latest_work) ",
    );

    let tuples = payload.iter().map(|(staker_address, shares)| {
        (
            currency_address.to_string(),
            staker_address.to_string(),
            last_blockheight as i64,
            shares,
        )
    });

    debug!("tuples last_state: {tuples:?}");

    query_builder.push_values(tuples, |mut b, tuple| {
        b.push_bind(tuple.0)
            .push_bind(tuple.1)
            .push_bind(tuple.2)
            .push_bind(tuple.3);
    });

    query_builder.push(
        " ON CONFLICT (currency_address, staker_address) 
        DO UPDATE SET latest_round = EXCLUDED.latest_round, latest_work = EXCLUDED.latest_work
        WHERE latest_state.currency_address = EXCLUDED.currency_address 
        AND latest_state.staker_address = EXCLUDED.staker_address",
    );

    query_builder.build().execute(&mut *tx).await?;

    tx.commit().await?;

    Ok(())
}

pub async fn store_work_v2(
    pool: &PgPool,
    currency_address: &Address,
    payload: HashMap<Address, Decimal>,
    last_blockheight: u64,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    for (staker_address, shares) in payload {
        sqlx::query_file!(
            "sql/store_work.sql",
            currency_address.to_string(),
            0,
            staker_address.to_string(),
            shares
        )
        .execute(&mut *tx)
        .await?;

        // TODO? there was a latest_round here that functions as a sort of
        // synchronization, where we keep track of the latest state of a subscriber.
        // it was only used in tests and to get the last round on startup
    }

    tx.commit().await?;

    Ok(())
}

pub async fn get_stakes_by_status(pool: &PgPool, status: String) -> Result<Vec<Stake>> {
    Ok(vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "sql/migrations")]
    async fn test_store_work(pool: PgPool) {
        let currency_address = Address::from_str("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq").unwrap();

        let mut payload = HashMap::new();

        payload.insert(
            Address::from_str("RJgnAuLfBwakw6VnBjzqQaksejtX8HEwNG").unwrap(),
            Decimal::from_f64_retain(1.23).unwrap(),
        );

        store_work_v2(&pool, &currency_address, payload, 1)
            .await
            .unwrap();

        let rows = sqlx::query("SELECT * FROM work")
            .fetch_all(&pool)
            .await
            .unwrap();

        let shares = rows.first().unwrap().get::<Decimal, &str>("shares");
        assert!(shares.is_sign_positive());
        assert_eq!(shares, Decimal::from_f64_retain(1.23).unwrap());
    }

    #[sqlx::test(migrations = "sql/migrations")]
    async fn test_multiple_store_work(pool: PgPool) {
        let currency_address = Address::from_str("iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq").unwrap();

        let mut payload = HashMap::new();

        payload.insert(
            Address::from_str("RJgnAuLfBwakw6VnBjzqQaksejtX8HEwNG").unwrap(),
            Decimal::from_f32_retain(1.23).unwrap(),
        );

        store_work_v2(&pool, &currency_address, payload, 1)
            .await
            .unwrap();

        let rows = sqlx::query("SELECT * FROM work")
            .fetch_all(&pool)
            .await
            .unwrap();

        let shares = rows.first().unwrap().get::<Decimal, &str>("shares");
        assert!(shares.is_sign_positive());
        assert_eq!(shares, Decimal::from_f32_retain(1.23).unwrap());

        let mut payload = HashMap::new();

        payload.insert(
            Address::from_str("RJgnAuLfBwakw6VnBjzqQaksejtX8HEwNG").unwrap(),
            Decimal::from_f32_retain(3.77).unwrap(),
        );

        store_work_v2(&pool, &currency_address, payload, 1)
            .await
            .unwrap();

        let rows = sqlx::query("SELECT * FROM work")
            .fetch_all(&pool)
            .await
            .unwrap();

        assert!(rows.len() == 1);

        let shares = rows.first().unwrap().get::<Decimal, &str>("shares");
        assert!(shares.is_sign_positive());

        assert_eq!(shares, Decimal::from_f32_retain(5.0).unwrap());
    }
}
