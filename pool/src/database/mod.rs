mod constants;

use std::collections::HashMap;
use std::str::FromStr;

use anyhow::Result;
use sqlx::types::Decimal;
use sqlx::{postgres::PgRow, PgPool, Postgres, QueryBuilder};
use sqlx::{Row, Transaction};
use vrsc_rpc::json::vrsc::{Address, Amount};

use crate::coinstaker::constants::{Stake, StakeStatus, Staker};
use crate::coinstaker::StakerStatus;
use crate::database::constants::{DbStake, DbStaker};

#[allow(unused)]
pub async fn store_staker(
    pool: &PgPool,
    staker: &Staker, // currency_address: &Address,
                     // identity: &Identity,
                     // status: StakerStatus,
                     // min_payout: u64,
) -> Result<()> {
    sqlx::query_file!(
        "sql/store_staker.sql",
        staker.currency_address.to_string(),
        staker.identity_address.to_string(),
        staker.identity_name,
        &staker.status as _,
        staker.min_payout.as_sat() as i64
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
    _last_blockheight: u64,
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

// used when a stake was found to be stale or stolen. Work that was assigned to a round
// before, should be moved back to round 0.
pub async fn move_work_to_round_zero(
    pool: &PgPool,
    currency_address: &Address,
    from_round: u64,
) -> Result<()> {
    sqlx::query!(
        "WITH round_to_move AS (
            SELECT currency_address, round, staker_address, shares
            FROM work 
            WHERE currency_address = $1 AND round = $2
        )
        INSERT INTO work (currency_address, round, staker_address, shares) 
        SELECT currency_address, 0, staker_address, shares
        FROM round_to_move
        ON CONFLICT (currency_address, round, staker_address)
        DO UPDATE SET shares = work.shares + EXCLUDED.shares",
        currency_address.to_string(),
        from_round as i64
    )
    .execute(pool)
    .await?;

    Ok(())
}

// used when a stake is found. Work until now needs to be moved to a round
// to be able to calculate a payout properly
async fn move_work_to_new_round(
    tx: &mut Transaction<'_, Postgres>,
    currency_address: &Address,
    from_round: u64,
    to_round: u64,
) -> Result<()> {
    sqlx::query!(
        "UPDATE work SET round = $3 WHERE currency_address = $1 AND round = $2",
        currency_address.to_string(),
        from_round as i64,
        to_round as i64
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub async fn store_new_stake(pool: &PgPool, stake: &Stake) -> Result<()> {
    let mut tx = pool.begin().await?;

    move_work_to_new_round(&mut tx, &stake.currency_address, 0, stake.block_height).await?;

    sqlx::query_file!(
        "sql/store_stake.sql",
        stake.currency_address.to_string(),
        stake.block_hash.to_string(),
        stake.block_height as i64,
        stake.amount.as_sat() as i64,
        stake.found_by.to_string(),
        stake.source_txid.to_string(),
        stake.source_vout_num as i32,
        stake.source_amount.as_sat() as i64,
        stake.status as _
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

pub async fn store_stake(pool: &PgPool, stake: &Stake) -> Result<()> {
    sqlx::query_file!(
        "sql/store_stake.sql",
        stake.currency_address.to_string(),
        stake.block_hash.to_string(),
        stake.block_height as i64,
        stake.amount.as_sat() as i64,
        stake.found_by.to_string(),
        stake.source_txid.to_string(),
        stake.source_vout_num as i32,
        stake.source_amount.as_sat() as i64,
        stake.status as _
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_stakes_by_status(pool: &PgPool, status: StakeStatus) -> Result<Vec<Stake>> {
    let rows = sqlx::query_as!(
        DbStake,
        r#"SELECT 
            currency_address,
            block_hash,
            block_height,
            amount,
            found_by,
            source_txid,
            source_vout_num,
            source_amount,
            status AS "status: _"
        FROM stakes 
        WHERE status = $1"#,
        status as StakeStatus
    )
    .try_map(Stake::try_from)
    .fetch_all(pool)
    .await?;

    Ok(rows)
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

        store_work(&pool, &currency_address, payload, 1)
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

        store_work(&pool, &currency_address, payload, 1)
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

        store_work(&pool, &currency_address, payload, 1)
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
