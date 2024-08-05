use std::collections::HashMap;
use std::str::FromStr;

use anyhow::Result;
use sqlx::postgres::PgRow;
use sqlx::types::Decimal;
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder, Row, Transaction};
use vrsc_rpc::bitcoin::Txid;
use vrsc_rpc::json::vrsc::{Address, Amount};

use super::constants::{DbPayoutMember, DbWorker};

use crate::coinstaker::constants::{Stake, StakeStatus, Staker};
use crate::coinstaker::StakerStatus;
use crate::database::constants::{DbStake, DbStaker};
use crate::payout_service::{Payout, PayoutMember, Worker};

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
        staker.min_payout.as_sat() as i64,
        staker.fee
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_stakers_by_identity_address(
    pool: &PgPool,
    currency_address: &Address,
    identity_addresses: &Vec<Address>,
) -> Result<Vec<Staker>> {
    let mut query_builder: QueryBuilder<Postgres> = sqlx::QueryBuilder::new(
        "SELECT *
        FROM stakers 
        WHERE (currency_address, identity_address) IN ",
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
            currency_address: Address::from_str(row.get("currency_address")).unwrap(),
            identity_address: Address::from_str(row.get("identity_address")).unwrap(),
            identity_name: row.get("identity_name"),
            min_payout: Amount::from_sat(row.get::<i64, &str>("min_payout") as u64),
            status: row.get::<StakerStatus, &str>("status"),
            fee: row.get("fee"),
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
            status AS "status: _",
            fee
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
            status AS "status: _", 
            fee
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
// rename: undo_work
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

pub async fn get_stake(
    pool: &PgPool,
    currency_address: &Address,
    block_height: u64,
) -> Result<Option<Stake>> {
    let value = sqlx::query_as!(
        DbStake,
        "SELECT currency_address,
            block_hash,
            block_height,
            amount,
            found_by,
            source_txid,
            source_vout_num,
            source_amount,
            status AS \"status: _\" 
        FROM stakes
        WHERE currency_address = $1 AND block_height = $2",
        currency_address.to_string(),
        block_height as i64
    )
    .try_map(Stake::try_from)
    .fetch_optional(pool)
    .await?;

    Ok(value)
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

pub async fn get_stakes_by_status(
    pool: &PgPool,
    currency_address: &Address,
    status: StakeStatus,
    from_id: Option<u64>,
) -> Result<Vec<Stake>> {
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
        WHERE currency_address = $1 AND 
            status = $2 AND 
            block_height > $3
        ORDER BY block_height ASC"#,
        currency_address.to_string(),
        status as StakeStatus,
        from_id.unwrap_or(0) as i64
    )
    .try_map(Stake::try_from)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

pub async fn get_stakes(
    pool: &PgPool,
    currency_address: &Address,
    from_height: Option<u64>,
) -> Result<Vec<Stake>> {
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
        WHERE currency_address = $1 AND 
            block_height > $2
        ORDER BY block_height ASC"#,
        currency_address.to_string(),
        from_height.unwrap_or(0) as i64
    )
    .try_map(Stake::try_from)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

pub async fn update_last_height(
    pool: &PgPool,
    currency_address: &Address,
    block_height: u64,
) -> Result<()> {
    let _res = sqlx::query!(
        "INSERT INTO synchronization (
            currency_address, 
            last_height
        ) VALUES ($1, $2) 
        ON CONFLICT (currency_address) 
        DO UPDATE 
        SET last_height = $2",
        currency_address.to_string(),
        block_height as i64
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_last_height(pool: &PgPool, currency_address: &Address) -> Result<Option<u64>> {
    let row = sqlx::query!(
        "SELECT last_height 
        FROM synchronization 
        WHERE currency_address = $1
        FOR UPDATE",
        currency_address.to_string()
    )
    .map(|r| r.last_height as u64)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

pub async fn get_workers_by_round(
    pool: &PgPool,
    currency_address: &Address,
    round: u64,
) -> Result<Vec<Worker>> {
    let workers = sqlx::query_as!(
        DbWorker,
        "SELECT identity_address, shares, fee FROM stakers s1
        JOIN work w1
        ON w1.staker_address = s1.identity_address AND s1.currency_address = w1.currency_address
        WHERE w1.round = $1 AND w1.currency_address = $2",
        round as i64,
        currency_address.to_string()
    )
    .try_map(Worker::try_from)
    .fetch_all(pool)
    .await?;

    Ok(workers)
}

pub async fn get_payout_sync_id(pool: &PgPool, currency_address: &Address) -> Result<Option<u64>> {
    let value = sqlx::query!(
        "SELECT last_payout_height 
        FROM synchronization 
        WHERE currency_address = $1 
        FOR UPDATE",
        currency_address.to_string()
    )
    .fetch_optional(pool)
    .await?;

    Ok(value.map(|row| row.last_payout_height as u64))
}

pub async fn update_last_payout_height(
    pool: &mut PgConnection,
    currency_address: &Address,
    block_height: u64,
) -> Result<()> {
    let _res = sqlx::query!(
        "INSERT INTO synchronization (
            currency_address, 
            last_payout_height
        ) VALUES ($1, $2) 
        ON CONFLICT (currency_address) 
        DO UPDATE 
        SET last_payout_height = $2",
        currency_address.to_string(),
        block_height as i64
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn store_payout(conn: &mut PgConnection, payout: &Payout) -> Result<()> {
    sqlx::query_file!(
        "sql/store_payout.sql",
        &payout.currency_address.to_string(),
        &payout.block_hash.to_string(),
        payout.block_height as i64,
        payout.amount.as_sat() as i64,
        &payout.total_work,
        payout.fee.as_sat() as i64,
        payout.paid.as_sat() as i64,
        payout.members.len() as i64
    )
    .execute(conn)
    .await?;

    Ok(())
}

pub async fn store_payout_member(
    conn: &mut PgConnection,
    payout_member: &PayoutMember,
) -> Result<()> {
    sqlx::query_file!(
        "sql/store_payout_member.sql",
        &payout_member.currency_address.to_string(),
        &payout_member.identity_address.to_string(),
        &payout_member.block_hash.to_string(),
        payout_member.block_height as i64,
        payout_member.shares,
        payout_member.reward.as_sat() as i64,
        payout_member.fee.as_sat() as i64,
        None::<&str>
    )
    .execute(conn)
    .await?;

    Ok(())
}

pub async fn get_payout_members(
    conn: &mut PgConnection,
    currency_address: &Address,
    identity_addresses: &[Address],
) -> Result<Vec<PayoutMember>> {
    let values = sqlx::query_as!(
        DbPayoutMember,
        "SELECT 
            currency_address,
            identity_address,
            block_hash,
            block_height,
            shares,
            reward,
            fee,
            txid
        FROM payout_members 
        WHERE currency_address = $1 
        AND identity_address IN (SELECT * FROM UNNEST($2::text[]))",
        currency_address.to_string(),
        &identity_addresses
            .iter()
            .map(|address| address.to_string())
            .collect::<Vec<_>>(),
    )
    .try_map(PayoutMember::try_from)
    .fetch_all(conn)
    .await?;

    Ok(values)
}

/// Get all payout members that have not been paid yet.
///
/// The payoutmembers are selected on their min_payout settings.
/// If a staker has left the pool, all remaining funds will be paid, disregarding
/// the min_payout settings of the staker.
///
/// The query locks the rows until the transaction is committed (or dropped on error).
pub async fn get_unpaid_payout_members(
    conn: &mut PgConnection,
    currency_address: &Address,
) -> Result<Vec<PayoutMember>> {
    let values = sqlx::query_as!(
        DbPayoutMember,
        "WITH pm_sum AS (
            SELECT currency_address, identity_address, SUM(reward) AS total_rewards
            FROM payout_members
            WHERE currency_address = $1
                AND txid is NULL
            GROUP BY currency_address, identity_address
        )
        SELECT 
            pm.currency_address,
            pm.identity_address,
            pm.block_hash,
            pm.block_height,
            pm.shares,
            pm.reward,
            pm.fee,
            pm.txid
        FROM payout_members pm
        JOIN pm_sum ON pm.currency_address = pm_sum.currency_address
            AND pm.identity_address = pm_sum.identity_address
            AND pm.txid IS NULL
        JOIN stakers s ON pm.currency_address = s.currency_address
            AND pm.identity_address = s.identity_address
        WHERE pm_sum.total_rewards > s.min_payout 
            OR s.status = 'INACTIVE'
        FOR UPDATE",
        currency_address.to_string(),
    )
    .try_map(PayoutMember::try_from)
    .fetch_all(conn)
    .await?;

    Ok(values)
}

pub async fn set_txid_payment_member(
    conn: &mut PgConnection,
    payout_member: &PayoutMember,
    txid: &Txid,
) -> Result<()> {
    sqlx::query!(
        "UPDATE payout_members SET txid = $3 WHERE currency_address = $1 AND block_height = $2",
        payout_member.currency_address.to_string(),
        payout_member.block_height as i64,
        txid.to_string()
    )
    .execute(&mut *conn)
    .await?;

    Ok(())
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
