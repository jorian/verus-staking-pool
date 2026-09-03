use std::collections::HashMap;
use std::str::FromStr;

use anyhow::Result;
use sqlx::postgres::PgRow;
use sqlx::types::Decimal;
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder, Row, Transaction};
use uuid::Uuid;
use vrsc_rpc::bitcoin::Txid;
use vrsc_rpc::json::vrsc::{Address, Amount};

use super::constants::{DbPayoutMember, DbWorker};

use crate::coinstaker::constants::{Stake, StakeStatus, Staker};
use crate::coinstaker::StakerStatus;
use crate::database::constants::{DbStake, DbStaker};
use crate::http::constants::WorkShare;
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
    if identity_addresses.is_empty() {
        return get_all_stakers(pool, currency_address).await;
    }

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

    let stakers = rows
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

    Ok(stakers)
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

pub async fn get_all_stakers(pool: &PgPool, currency_address: &Address) -> Result<Vec<Staker>> {
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
        ORDER BY identity_name"#,
        currency_address.to_string()
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

pub async fn update_staker_fee(
    pool: &PgPool,
    currency_address: &Address,
    identity_address: &Address,
    fee: Decimal,
) -> Result<Option<Staker>> {
    let staker = sqlx::query_file_as!(
        DbStaker,
        "sql/update_staker_fee.sql",
        currency_address.to_string(),
        identity_address.to_string(),
        fee
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
    blockheight: u64,
    extra_snapshot: Decimal,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let mut total = extra_snapshot;

    for (staker_address, shares) in payload {
        total += shares;
        sqlx::query_file!(
            "sql/store_work.sql",
            currency_address.to_string(),
            0,
            staker_address.to_string(),
            shares
        )
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query_file!(
        "sql/store_work_snapshot.sql",
        currency_address.to_string(),
        blockheight as i64,
        total
    )
    .execute(&mut *tx)
    .await?;

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

/*INSERT INTO work(
    currency_address,
    round,
    staker_address,
    shares
) VALUES ($1, $2, $3, $4)
ON CONFLICT ON CONSTRAINT work_pkey
DO UPDATE
SET shares = work.shares + EXCLUDED.shares
WHERE work.currency_address = EXCLUDED.currency_address
    AND work.round = EXCLUDED.round
    AND work.staker_address = EXCLUDED.staker_address
 */

// keeps the work in `from_round`
async fn copy_work_to_new_round(
    tx: &mut Transaction<'_, Postgres>,
    currency_address: &Address,
    from_round: u64,
    to_round: u64,
    staker_address: &Address,
) -> Result<()> {
    sqlx::query!(
        "WITH round_to_copy AS (
            SELECT shares
            FROM work 
            WHERE currency_address = $1 AND round = $2
        )
        INSERT INTO work (currency_address, round, staker_address, shares) 
        SELECT $1, $3, $4, shares
        FROM round_to_copy",
        currency_address.to_string(),
        from_round as i64,
        to_round as i64,
        staker_address.to_string()
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

pub async fn store_new_stake(pool: &PgPool, stake: &Stake, move_work: bool) -> Result<()> {
    let mut tx = pool.begin().await?;

    if move_work {
        move_work_to_new_round(&mut tx, &stake.currency_address, 0, stake.block_height).await?;
    } else {
        copy_work_to_new_round(
            &mut tx,
            &stake.currency_address,
            0,
            stake.block_height,
            &stake.found_by,
        )
        .await?;
    }

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

/// Matured stakes that do not yet have a `payouts` row.
///
/// Used instead of a `last_payout_height` cursor so a later-height stake
/// cannot permanently skip an earlier one.
pub async fn get_matured_stakes_without_payout(
    pool: &PgPool,
    currency_address: &Address,
) -> Result<Vec<Stake>> {
    let rows = sqlx::query_as!(
        DbStake,
        r#"SELECT
            s.currency_address,
            s.block_hash,
            s.block_height,
            s.amount,
            s.found_by,
            s.source_txid,
            s.source_vout_num,
            s.source_amount,
            s.status AS "status: _"
        FROM stakes s
        WHERE s.currency_address = $1
            AND s.status = $2
            AND NOT EXISTS (
                SELECT 1
                FROM payouts p
                WHERE p.currency_address = s.currency_address
                    AND p.block_hash = s.block_hash
            )
        ORDER BY s.block_height ASC"#,
        currency_address.to_string(),
        StakeStatus::Matured as StakeStatus
    )
    .try_map(Stake::try_from)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Stakes whose spent UTXO is still missing from `listunspent(minconf=150)`.
///
/// Find height N: UTXO gone immediately. New coinbase is eligible at height
/// N+149 (150 confirmations). Credit `source_amount` for N..=N+148, i.e.
/// `N > B-149 && N <= B`. The find block itself is passed into `add_work`
/// in-memory because it is not stored until after work is written.
pub async fn get_stakes_to_compensate(
    pool: &PgPool,
    currency_address: &Address,
    block_height: i64,
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
            block_height > ($2 - 149) AND
            block_height <= $2 AND
            (status = 'MATURED' OR status = 'MATURING')
        ORDER BY block_height ASC"#,
        currency_address.to_string(),
        block_height as i64
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

/// Newest-first page for the public HTTP API (`block_height < before_height`).
pub async fn get_stakes_page(
    pool: &PgPool,
    currency_address: &Address,
    status: Option<StakeStatus>,
    before_height: Option<u64>,
    limit: u32,
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
        WHERE currency_address = $1
            AND ($2::stake_status IS NULL OR status = $2)
            AND ($3::bigint IS NULL OR block_height < $3)
        ORDER BY block_height DESC
        LIMIT $4"#,
        currency_address.to_string(),
        status as Option<StakeStatus>,
        before_height.map(|h| h as i64),
        limit as i64
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

pub async fn get_work(pool: &PgPool, currency_address: &Address) -> Result<Vec<WorkShare>> {
    let rows = sqlx::query!(
        r#"SELECT staker_address, shares
        FROM work
        WHERE currency_address = $1 AND round = 0
        ORDER BY shares DESC"#,
        currency_address.to_string()
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(WorkShare {
                identity_address: Address::from_str(&row.staker_address)
                    .map_err(|e| anyhow::anyhow!("{e}"))?,
                shares: row.shares,
            })
        })
        .collect()
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

pub async fn get_all_payout_members(
    pool: &PgPool,
    currency_address: &Address,
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
        ORDER BY block_height ASC",
        currency_address.to_string(),
    )
    .try_map(PayoutMember::try_from)
    .fetch_all(pool)
    .await?;

    Ok(values)
}

/// Newest-first page. Empty `identity_addresses` means every member.
pub async fn get_payout_members_page(
    pool: &PgPool,
    currency_address: &Address,
    identity_addresses: &[Address],
    before_height: Option<u64>,
    limit: u32,
) -> Result<Vec<PayoutMember>> {
    let ids: Vec<String> = identity_addresses.iter().map(|a| a.to_string()).collect();
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
            AND (CARDINALITY($2::text[]) = 0 OR identity_address = ANY($2))
            AND ($3::bigint IS NULL OR block_height < $3)
        ORDER BY block_height DESC
        LIMIT $4",
        currency_address.to_string(),
        &ids,
        before_height.map(|h| h as i64),
        limit as i64
    )
    .try_map(PayoutMember::try_from)
    .fetch_all(pool)
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
                AND payment_batch_id IS NULL
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
            AND pm.payment_batch_id IS NULL
        JOIN stakers s ON pm.currency_address = s.currency_address
            AND pm.identity_address = s.identity_address
        WHERE pm_sum.total_rewards >= s.min_payout 
            OR s.status = 'INACTIVE'
        FOR UPDATE",
        currency_address.to_string(),
    )
    .try_map(PayoutMember::try_from)
    .fetch_all(&mut *conn)
    .await?;

    Ok(values)
}

pub struct InFlightPayoutBatch {
    pub batch_id: Uuid,
    pub opid: Option<String>,
    pub members: Vec<PayoutMember>,
}

struct DbInFlightPayoutMember {
    currency_address: String,
    identity_address: String,
    block_hash: String,
    block_height: i64,
    shares: Decimal,
    reward: i64,
    fee: i64,
    txid: Option<String>,
    payment_batch_id: Uuid,
    payment_opid: Option<String>,
}

pub async fn get_in_flight_payout_batches(
    pool: &PgPool,
    currency_address: &Address,
) -> Result<Vec<InFlightPayoutBatch>> {
    let rows = sqlx::query_as!(
        DbInFlightPayoutMember,
        r#"SELECT
            currency_address,
            identity_address,
            block_hash,
            block_height,
            shares,
            reward,
            fee,
            txid,
            payment_batch_id AS "payment_batch_id!",
            payment_opid
        FROM payout_members
        WHERE currency_address = $1
            AND txid IS NULL
            AND payment_batch_id IS NOT NULL
        ORDER BY payment_batch_id"#,
        currency_address.to_string(),
    )
    .fetch_all(pool)
    .await?;

    let mut batches: HashMap<Uuid, InFlightPayoutBatch> = HashMap::new();
    for row in rows {
        let member = PayoutMember::try_from(DbPayoutMember {
            currency_address: row.currency_address,
            identity_address: row.identity_address,
            block_hash: row.block_hash,
            block_height: row.block_height,
            shares: row.shares,
            reward: row.reward,
            fee: row.fee,
            txid: row.txid,
        })?;

        batches
            .entry(row.payment_batch_id)
            .and_modify(|batch| batch.members.push(member.clone()))
            .or_insert_with(|| InFlightPayoutBatch {
                batch_id: row.payment_batch_id,
                opid: row.payment_opid.clone(),
                members: vec![member],
            });
    }

    Ok(batches.into_values().collect())
}

pub async fn claim_payout_members(
    conn: &mut PgConnection,
    batch_id: Uuid,
    members: &[PayoutMember],
) -> Result<()> {
    for member in members {
        let result = sqlx::query!(
            r#"UPDATE payout_members
            SET payment_batch_id = $1
            WHERE currency_address = $2
                AND identity_address = $3
                AND block_hash = $4
                AND txid IS NULL
                AND payment_batch_id IS NULL"#,
            batch_id,
            member.currency_address.to_string(),
            member.identity_address.to_string(),
            member.block_hash.to_string(),
        )
        .execute(&mut *conn)
        .await?;

        if result.rows_affected() != 1 {
            anyhow::bail!(
                "failed to claim payout member {} at height {}",
                member.identity_address,
                member.block_height
            );
        }
    }

    Ok(())
}

pub async fn set_payment_opid_for_batch(pool: &PgPool, batch_id: Uuid, opid: &str) -> Result<()> {
    sqlx::query!(
        r#"UPDATE payout_members
        SET payment_opid = $2
        WHERE payment_batch_id = $1
            AND txid IS NULL"#,
        batch_id,
        opid,
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn set_txid_for_batch(pool: &PgPool, batch_id: Uuid, txid: &Txid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE payout_members
        SET txid = $2
        WHERE payment_batch_id = $1
            AND txid IS NULL"#,
        batch_id,
        txid.to_string(),
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn unclaim_payout_batch(pool: &PgPool, batch_id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE payout_members
        SET payment_batch_id = NULL,
            payment_opid = NULL
        WHERE payment_batch_id = $1
            AND txid IS NULL"#,
        batch_id,
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_number_of_matured_stakes(
    conn: &PgPool,
    currency_address: &Address,
) -> Result<i64> {
    let res: Option<i64> = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM stakes WHERE currency_address = $1 AND status = 'MATURED'",
        currency_address.to_string()
    )
    .fetch_one(conn)
    .await?;

    Ok(res.unwrap_or(0))
}

pub async fn get_number_of_active_stakers(
    conn: &PgPool,
    currency_address: &Address,
) -> Result<i64> {
    let res: Option<i64> = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM stakers WHERE currency_address = $1 AND status = 'ACTIVE'",
        currency_address.to_string()
    )
    .fetch_one(conn)
    .await?;

    Ok(res.unwrap_or(0))
}

pub async fn get_total_rewards(conn: &PgPool, currency_address: &Address) -> Result<Amount> {
    let res: Option<Amount> = sqlx::query!(
        "SELECT SUM(reward)::bigint as total 
        FROM payout_members 
        WHERE 
            currency_address = $1 AND 
            txid is not null",
        currency_address.to_string()
    )
    .fetch_one(conn)
    .await?
    .total
    .map(|r| Amount::from_sat(r as u64));

    Ok(res.unwrap_or(Amount::ZERO))
}

const HISTORY_MAX_POINTS: usize = 240;

fn take_evenly<T: Clone>(items: &[T], max: usize) -> Vec<T> {
    if items.len() <= max || max < 2 {
        return items.to_vec();
    }
    let last = items.len() - 1;
    (0..max)
        .map(|i| items[i * last / (max - 1)].clone())
        .collect()
}

/// Eligible pool staking supply at each block height (one snapshot per block).
pub async fn get_work_history(
    pool: &PgPool,
    currency_address: &Address,
) -> Result<Vec<crate::http::constants::StakingBalancePoint>> {
    let rows = sqlx::query!(
        r#"SELECT height, shares
        FROM work_snapshots
        WHERE currency_address = $1
            AND height > (
                SELECT COALESCE(MAX(height), 0) - 40320
                FROM work_snapshots
                WHERE currency_address = $1
            )
        ORDER BY height"#,
        currency_address.to_string()
    )
    .fetch_all(pool)
    .await?;

    let last = rows.last().map(|row| row.height);
    let points: Vec<_> = rows
        .into_iter()
        .map(|row| crate::http::constants::StakingBalancePoint {
            height: row.height,
            sats: row.shares.round_dp(0).to_string(),
            current: last == Some(row.height),
        })
        .collect();

    Ok(points)
}

/// Cumulative count of pool stakes by block height.
pub async fn get_stake_count_history(
    pool: &PgPool,
    currency_address: &Address,
) -> Result<Vec<crate::http::constants::StakeCountPoint>> {
    let rows = sqlx::query!(
        r#"SELECT block_height, COUNT(*) AS "n!"
        FROM stakes
        WHERE currency_address = $1
        GROUP BY block_height
        ORDER BY block_height"#,
        currency_address.to_string()
    )
    .fetch_all(pool)
    .await?;

    let mut acc = 0i64;
    let points: Vec<_> = rows
        .into_iter()
        .map(|row| {
            acc += row.n;
            crate::http::constants::StakeCountPoint {
                height: row.block_height,
                count: acc,
            }
        })
        .collect();

    Ok(take_evenly(&points, HISTORY_MAX_POINTS))
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

        store_work(&pool, &currency_address, payload, 1, Decimal::ZERO)
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

        store_work(&pool, &currency_address, payload, 1, Decimal::ZERO)
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

        store_work(&pool, &currency_address, payload, 1, Decimal::ZERO)
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

    #[sqlx::test(migrations = "sql/migrations")]
    async fn claimed_members_are_excluded_from_unpaid(pool: PgPool) {
        let currency = Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap();
        let identity = Address::from_str("iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU").unwrap();
        let block_hash = "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0";

        sqlx::query(
            "INSERT INTO stakers (
                currency_address, identity_address, identity_name, status, min_payout, fee
            ) VALUES ($1, $2, 'alice', 'ACTIVE', 0, 0)",
        )
        .bind(currency.to_string())
        .bind(identity.to_string())
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO payout_members (
                currency_address, identity_address, block_hash, block_height,
                shares, reward, fee, txid
            ) VALUES ($1, $2, $3, 1, 1, 100000000, 0, NULL)",
        )
        .bind(currency.to_string())
        .bind(identity.to_string())
        .bind(block_hash)
        .execute(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let unpaid = get_unpaid_payout_members(&mut tx, &currency).await.unwrap();
        assert_eq!(unpaid.len(), 1);

        let batch_id = Uuid::new_v4();
        claim_payout_members(&mut tx, batch_id, &unpaid)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        let unpaid_after = get_unpaid_payout_members(&mut tx, &currency).await.unwrap();
        tx.commit().await.unwrap();
        assert!(unpaid_after.is_empty());

        let in_flight = get_in_flight_payout_batches(&pool, &currency)
            .await
            .unwrap();
        assert_eq!(in_flight.len(), 1);
        assert_eq!(in_flight[0].batch_id, batch_id);
        assert!(in_flight[0].opid.is_none());

        set_payment_opid_for_batch(&pool, batch_id, "opid-test")
            .await
            .unwrap();
        let in_flight = get_in_flight_payout_batches(&pool, &currency)
            .await
            .unwrap();
        assert_eq!(in_flight[0].opid.as_deref(), Some("opid-test"));

        let txid =
            Txid::from_str("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                .unwrap();
        set_txid_for_batch(&pool, batch_id, &txid).await.unwrap();

        let in_flight = get_in_flight_payout_batches(&pool, &currency)
            .await
            .unwrap();
        assert!(in_flight.is_empty());

        let mut tx = pool.begin().await.unwrap();
        let unpaid_paid = get_unpaid_payout_members(&mut tx, &currency).await.unwrap();
        tx.commit().await.unwrap();
        assert!(unpaid_paid.is_empty());
    }

    async fn insert_staker(
        pool: &PgPool,
        currency: &Address,
        identity: &Address,
        name: &str,
        status: &str,
    ) {
        sqlx::query(
            "INSERT INTO stakers (
                currency_address, identity_address, identity_name, status, min_payout, fee
            ) VALUES ($1, $2, $3, $4::staker_status, 0, 0)",
        )
        .bind(currency.to_string())
        .bind(identity.to_string())
        .bind(name)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test(migrations = "sql/migrations")]
    async fn empty_identity_list_returns_all_stakers(pool: PgPool) {
        let currency = Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap();
        let alice = Address::from_str("iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU").unwrap();
        let bob = Address::from_str("iGLN3bFv6uY2HAgQgVwiGriTRgQmTyJrwi").unwrap();
        insert_staker(&pool, &currency, &alice, "alice", "ACTIVE").await;
        insert_staker(&pool, &currency, &bob, "bob", "INACTIVE").await;

        let all = get_stakers_by_identity_address(&pool, &currency, &vec![])
            .await
            .unwrap();
        assert_eq!(all.len(), 2);

        let named = get_all_stakers(&pool, &currency).await.unwrap();
        assert_eq!(named[0].identity_name, "alice");
        assert_eq!(named[1].identity_name, "bob");
    }

    #[sqlx::test(migrations = "sql/migrations")]
    async fn stakes_page_respects_limit_and_before_height(pool: PgPool) {
        let currency = Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap();
        let found_by = Address::from_str("iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU").unwrap();
        let txid = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        for (i, height) in [10i64, 20, 30].iter().enumerate() {
            sqlx::query(
                "INSERT INTO stakes (
                    currency_address, block_hash, block_height, amount, found_by,
                    source_txid, source_vout_num, source_amount, status
                ) VALUES ($1, $2, $3, 1, $4, $5, 0, 1, 'MATURED')",
            )
            .bind(currency.to_string())
            .bind(format!(
                "000000000000000000000000000000000000000000000000000000000000000{i}"
            ))
            .bind(height)
            .bind(found_by.to_string())
            .bind(txid)
            .execute(&pool)
            .await
            .unwrap();
        }

        let unbounded = get_stakes(&pool, &currency, None).await.unwrap();
        assert_eq!(
            unbounded.iter().map(|s| s.block_height).collect::<Vec<_>>(),
            vec![10, 20, 30]
        );

        let page = get_stakes_page(&pool, &currency, None, None, 2)
            .await
            .unwrap();
        assert_eq!(
            page.iter().map(|s| s.block_height).collect::<Vec<_>>(),
            vec![30, 20]
        );

        let next = get_stakes_page(&pool, &currency, None, Some(20), 2)
            .await
            .unwrap();
        assert_eq!(
            next.iter().map(|s| s.block_height).collect::<Vec<_>>(),
            vec![10]
        );
    }

    #[sqlx::test(migrations = "sql/migrations")]
    async fn work_round_zero_is_current_unpaid_shares(pool: PgPool) {
        let currency = Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap();
        let alice = Address::from_str("iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU").unwrap();
        let mut payload = HashMap::new();
        payload.insert(alice.clone(), Decimal::from_f64_retain(1.5).unwrap());
        store_work(&pool, &currency, payload, 1, Decimal::ZERO)
            .await
            .unwrap();

        let work = get_work(&pool, &currency).await.unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].identity_address, alice);
        assert_eq!(work[0].shares, Decimal::from_f64_retain(1.5).unwrap());
    }

    #[sqlx::test(migrations = "sql/migrations")]
    async fn work_history_is_staking_balance_by_height(pool: PgPool) {
        let currency = Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap();
        let alice = Address::from_str("iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU").unwrap();
        let mut first = HashMap::new();
        first.insert(alice.clone(), Decimal::from_f64_retain(100.0).unwrap());
        store_work(&pool, &currency, first, 10, Decimal::ZERO)
            .await
            .unwrap();
        let mut second = HashMap::new();
        second.insert(alice, Decimal::from_f64_retain(120.0).unwrap());
        store_work(&pool, &currency, second, 11, Decimal::from(15))
            .await
            .unwrap();

        let history = get_work_history(&pool, &currency).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].height, 10);
        assert_eq!(history[0].sats, "100");
        assert!(!history[0].current);
        assert_eq!(history[1].height, 11);
        assert_eq!(history[1].sats, "135");
        assert!(history[1].current);
        let work = get_work(&pool, &currency).await.unwrap();
        assert_eq!(work[0].shares, Decimal::from_f64_retain(220.0).unwrap());
    }

    #[sqlx::test(migrations = "sql/migrations")]
    async fn compensate_window_is_find_height_through_plus_148(pool: PgPool) {
        let currency = Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap();
        let found_by = Address::from_str("iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU").unwrap();
        let n = 1000i64;
        sqlx::query(
            "INSERT INTO stakes (
                currency_address, block_hash, block_height, amount, found_by,
                source_txid, source_vout_num, source_amount, status
            ) VALUES ($1, $2, $3, 1, $4, $5, 0, 50000000, 'MATURING')",
        )
        .bind(currency.to_string())
        .bind("00000000000000000000000000000000000000000000000000000000000000aa")
        .bind(n)
        .bind(found_by.to_string())
        .bind("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        .execute(&pool)
        .await
        .unwrap();

        let ids = |stakes: Vec<Stake>| {
            stakes
                .into_iter()
                .map(|s| s.block_height)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            ids(get_stakes_to_compensate(&pool, &currency, n).await.unwrap()),
            vec![1000]
        );
        assert_eq!(
            ids(get_stakes_to_compensate(&pool, &currency, n + 1)
                .await
                .unwrap()),
            vec![1000]
        );
        assert_eq!(
            ids(get_stakes_to_compensate(&pool, &currency, n + 148)
                .await
                .unwrap()),
            vec![1000]
        );
        assert!(
            get_stakes_to_compensate(&pool, &currency, n + 149)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            get_stakes_to_compensate(&pool, &currency, n - 1)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test(migrations = "sql/migrations")]
    async fn unclaim_returns_members_to_unpaid(pool: PgPool) {
        let currency = Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap();
        let identity = Address::from_str("iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU").unwrap();
        let block_hash = "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0";

        sqlx::query(
            "INSERT INTO stakers (
                currency_address, identity_address, identity_name, status, min_payout, fee
            ) VALUES ($1, $2, 'alice', 'INACTIVE', 100000000, 0)",
        )
        .bind(currency.to_string())
        .bind(identity.to_string())
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO payout_members (
                currency_address, identity_address, block_hash, block_height,
                shares, reward, fee, txid
            ) VALUES ($1, $2, $3, 1, 1, 1, 0, NULL)",
        )
        .bind(currency.to_string())
        .bind(identity.to_string())
        .bind(block_hash)
        .execute(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let unpaid = get_unpaid_payout_members(&mut tx, &currency).await.unwrap();
        let batch_id = Uuid::new_v4();
        claim_payout_members(&mut tx, batch_id, &unpaid)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        unclaim_payout_batch(&pool, batch_id).await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        let unpaid = get_unpaid_payout_members(&mut tx, &currency).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(unpaid.len(), 1);
        assert!(get_in_flight_payout_batches(&pool, &currency)
            .await
            .unwrap()
            .is_empty());
    }

    #[sqlx::test(migrations = "sql/migrations")]
    async fn exact_min_payout_is_paid(pool: PgPool) {
        let currency = Address::from_str("i5w5MuNik5NtLcYmNzcvaoixooEebB6MGV").unwrap();
        let identity = Address::from_str("iB5PRXMHLYcNtM8dfLB6KwfJrHU2mKDYuU").unwrap();
        let block_hash = "00000000000797cb62652d5901ab30e907f9a5657947eba15f1c9e7e19abe2e0";
        let min_payout: i64 = 100_000_000;

        sqlx::query(
            "INSERT INTO stakers (
                currency_address, identity_address, identity_name, status, min_payout, fee
            ) VALUES ($1, $2, 'alice', 'ACTIVE', $3, 0)",
        )
        .bind(currency.to_string())
        .bind(identity.to_string())
        .bind(min_payout)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO payout_members (
                currency_address, identity_address, block_hash, block_height,
                shares, reward, fee, txid
            ) VALUES ($1, $2, $3, 1, 1, $4, 0, NULL)",
        )
        .bind(currency.to_string())
        .bind(identity.to_string())
        .bind(block_hash)
        .bind(min_payout)
        .execute(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let unpaid = get_unpaid_payout_members(&mut tx, &currency).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(unpaid.len(), 1);
        assert_eq!(unpaid[0].reward.as_sat(), min_payout as u64);
    }
}
