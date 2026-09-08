use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::Result;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemHandle};
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;
use vrsc_rpc::{
    bitcoin::Txid,
    client::{Client, RpcApi, SendCurrencyOutput},
    json::{vrsc::Address, vrsc::Amount, ZOperationStatusResult},
};

use crate::{
    coinstaker::{
        constants::Stake,
        http::{PayoutStuckMember, Webhook, WebhookMessage},
        ChainConfig, PayoutConfig as PayoutServiceConfig,
    },
    database::{self, InFlightPayoutBatch},
};

use super::{payout::Payout, PayoutMember};

const SENDCURRENCY_WAIT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SENDCURRENCY_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct Service {
    database: PgPool,
    config: PayoutServiceConfig,
    chain_id: Address,
    currency_name: String,
    pool_address: Address,
    chain_config: ChainConfig,
    webhooks: Webhook,
    notified_stuck_batches: Mutex<HashSet<Uuid>>,
    /// Set after allocating a matured stake so leavers are paid once in-flight
    /// (if any) has cleared. Not used for the periodic active send.
    pending_inactive_payout: Mutex<bool>,
}

impl Service {
    pub fn new(
        config: PayoutServiceConfig,
        database: PgPool,
        chain_id: Address,
        currency_name: String,
        pool_address: Address,
        chain_config: ChainConfig,
        webhook_endpoints: Vec<url::Url>,
    ) -> Result<Self> {
        Ok(Self {
            database,
            config,
            chain_id,
            currency_name,
            pool_address,
            chain_config,
            webhooks: Webhook::new(webhook_endpoints)?,
            notified_stuck_batches: Mutex::new(HashSet::new()),
            pending_inactive_payout: Mutex::new(false),
        })
    }

    fn workers_have_shares(workers: &[crate::payout_service::Worker]) -> bool {
        workers.iter().any(|w| w.shares > Decimal::ZERO)
    }

    async fn skip_payout_without_work(&self, stake: &Stake) {
        warn!(
            height = stake.block_height,
            hash = %stake.block_hash,
            amount = %stake.amount,
            "matured stake has no work; not creating a payout"
        );
        self.webhooks
            .send(WebhookMessage::PayoutSkippedNoWork {
                currency_address: stake.currency_address.clone(),
                currency_name: self.currency_name.clone(),
                hash: stake.block_hash,
                height: stake.block_height,
                amount: stake.amount,
            })
            .await;
    }

    async fn new_manual_payout(&self, stake: &Stake) -> Result<()> {
        let workers =
            database::get_workers_by_round(&self.database, &self.chain_id, stake.block_height)
                .await?;

        if !Self::workers_have_shares(&workers) {
            self.skip_payout_without_work(stake).await;
            anyhow::bail!(
                "stake at height {} has no work; refusing to create an empty payout",
                stake.block_height
            );
        }

        let mut tx = self.database.begin().await?;

        let payout = Payout::new(&stake, workers, Decimal::ZERO)?;

        database::store_payout(&mut tx, &payout).await?;

        for member in payout.members {
            database::store_payout_member(&mut tx, &member).await?;
        }

        database::update_last_payout_height(&mut tx, &self.chain_id, stake.block_height).await?;

        tx.commit().await?;

        Ok(())
    }

    async fn new_payout(&self) -> Result<()> {
        let stakes =
            database::get_matured_stakes_without_payout(&self.database, &self.chain_id).await?;
        let mut allocated = false;

        for stake in stakes {
            let workers =
                database::get_workers_by_round(&self.database, &self.chain_id, stake.block_height)
                    .await?;

            if !Self::workers_have_shares(&workers) {
                self.skip_payout_without_work(&stake).await;
                continue;
            }

            let mut tx = self.database.begin().await?;

            let payout = Payout::new(&stake, workers, Decimal::ZERO)?;

            database::store_payout(&mut tx, &payout).await?;

            for member in payout.members {
                database::store_payout_member(&mut tx, &member).await?;
            }

            database::update_last_payout_height(&mut tx, &self.chain_id, stake.block_height)
                .await?;

            tx.commit().await?;
            allocated = true;
        }

        if allocated {
            *self.pending_inactive_payout.lock().unwrap() = true;
            self.send_unpaid_for_inactive().await?;
        }

        Ok(())
    }

    async fn has_in_flight(&self) -> Result<bool> {
        Ok(!database::get_in_flight_payout_batches(&self.database, &self.chain_id)
            .await?
            .is_empty())
    }

    async fn resume_all_in_flight(&self) -> Result<()> {
        let in_flight =
            database::get_in_flight_payout_batches(&self.database, &self.chain_id).await?;
        if in_flight.is_empty() {
            return Ok(());
        }
        if in_flight.len() > 1 {
            error!(
                n_batches = in_flight.len(),
                "multiple in-flight payout batches; not starting a new send"
            );
        }
        for batch in in_flight {
            self.resume_in_flight(batch).await?;
        }
        Ok(())
    }

    /// Pay every inactive staker with unpaid rows in one `sendcurrency`.
    ///
    /// No-op while another batch is in flight (the send loop retries after
    /// that batch finishes). Ignores `min_payout`.
    async fn send_unpaid_for_inactive(&self) -> Result<()> {
        if self.has_in_flight().await? {
            return Ok(());
        }

        let mut tx = self.database.begin().await?;
        let unpaid =
            database::get_unpaid_payout_members_for_inactive(&mut tx, &self.chain_id).await?;
        if unpaid.is_empty() {
            tx.commit().await?;
            *self.pending_inactive_payout.lock().unwrap() = false;
            return Ok(());
        }

        let batch_id = Uuid::new_v4();
        database::claim_payout_members(&mut tx, batch_id, &unpaid).await?;
        tx.commit().await?;

        info!(
            n_members = unpaid.len(),
            n_identities = unpaid
                .iter()
                .map(|m| m.identity_address.to_string())
                .collect::<HashSet<_>>()
                .len(),
            %batch_id,
            "sending leave payout for inactive stakers"
        );
        let result = self.submit_claimed_batch(batch_id, &unpaid).await;
        if result.is_ok() {
            *self.pending_inactive_payout.lock().unwrap() = false;
        }
        result
    }

    async fn send_active_unpaid(&self) -> Result<()> {
        let mut tx = self.database.begin().await?;
        let unpaid_payout_members =
            database::get_unpaid_payout_members(&mut tx, &self.chain_id).await?;

        if unpaid_payout_members.is_empty() {
            tx.commit().await?;
            return Ok(());
        }

        let batch_id = Uuid::new_v4();
        database::claim_payout_members(&mut tx, batch_id, &unpaid_payout_members).await?;
        tx.commit().await?;

        self.submit_claimed_batch(batch_id, &unpaid_payout_members)
            .await
    }

    async fn submit_claimed_batch(
        &self,
        batch_id: Uuid,
        unpaid_payout_members: &[PayoutMember],
    ) -> Result<()> {
        let outputs = prepare_payment(&unpaid_payout_members.to_vec())?;
        debug!(n_outputs = outputs.len(), ?outputs, "sending outputs");
        let client: Client = (&self.chain_config).try_into()?;

        let opid = match client.send_currency(&self.pool_address.to_string(), outputs, None, None) {
            Ok(opid) => opid,
            Err(e) => {
                error!(%batch_id, error = ?e, "sendcurrency RPC failed before an opid was returned; unclaiming");
                database::unclaim_payout_batch(&self.database, batch_id).await?;
                return Err(e.into());
            }
        };

        database::set_payment_opid_for_batch(&self.database, batch_id, &opid).await?;

        self.finish_submitted_batch(batch_id, &opid).await
    }

    async fn send_unsent_payouts(&self) -> Result<()> {
        self.resume_all_in_flight().await?;
        if self.has_in_flight().await? {
            return Ok(());
        }

        if *self.pending_inactive_payout.lock().unwrap() {
            self.send_unpaid_for_inactive().await?;
            if self.has_in_flight().await? {
                return Ok(());
            }
        }

        self.send_active_unpaid().await
    }

    async fn resume_in_flight(&self, batch: InFlightPayoutBatch) -> Result<()> {
        let Some(opid) = batch.opid.as_deref() else {
            error!(
                batch_id = %batch.batch_id,
                n_members = batch.members.len(),
                members = ?batch.members.iter().map(|m| m.identity_address.to_string()).collect::<Vec<_>>(),
                "payout batch claimed with no daemon opid; refusing to send again"
            );
            self.notify_stuck_batch(&batch, "claimed_without_opid")
                .await;
            return Ok(());
        };

        let client: Client = (&self.chain_config).try_into()?;
        let operation_status = client.z_get_operation_status(vec![opid])?;

        match classify_operation_status(operation_status.first().and_then(|s| s.as_ref())) {
            OperationOutcome::Pending => {
                info!(batch_id = %batch.batch_id, opid, "resuming in-flight sendcurrency");
                self.finish_submitted_batch(batch.batch_id, opid).await
            }
            OperationOutcome::Success(txid) => {
                database::set_txid_for_batch(&self.database, batch.batch_id, &txid).await?;
                info!(?txid, batch_id = %batch.batch_id, "in-flight sendcurrency already succeeded");
                Ok(())
            }
            OperationOutcome::Failed(msg) => {
                error!(batch_id = %batch.batch_id, opid, error = %msg, "in-flight sendcurrency failed; unclaiming");
                database::unclaim_payout_batch(&self.database, batch.batch_id).await?;
                Ok(())
            }
            OperationOutcome::Unknown => {
                error!(
                    batch_id = %batch.batch_id,
                    opid,
                    "daemon does not know this opid (restarted?); refusing to send again"
                );
                self.notify_stuck_batch(&batch, "unknown_opid").await;
                Ok(())
            }
        }
    }

    async fn finish_submitted_batch(&self, batch_id: Uuid, opid: &str) -> Result<()> {
        let client: Client = (&self.chain_config).try_into()?;

        match wait_for_sendcurrency_finish(&client, opid).await {
            Ok(txid) => {
                database::set_txid_for_batch(&self.database, batch_id, &txid).await?;
                info!(?txid, %batch_id, "Sent payment");
                Ok(())
            }
            Err(WaitError::Failed(msg)) => {
                error!(%batch_id, opid, error = %msg, "sendcurrency failed; unclaiming for retry");
                database::unclaim_payout_batch(&self.database, batch_id).await?;
                Ok(())
            }
            Err(WaitError::Timeout) => {
                warn!(%batch_id, opid, "sendcurrency wait timed out; will resume later");
                Ok(())
            }
            Err(WaitError::Rpc(e)) => {
                error!(%batch_id, opid, error = ?e, "sendcurrency status RPC failed; will resume later");
                Ok(())
            }
        }
    }

    async fn notify_stuck_batch(&self, batch: &InFlightPayoutBatch, reason: &str) {
        {
            let notified = self.notified_stuck_batches.lock().unwrap();
            if notified.contains(&batch.batch_id) {
                return;
            }
        }

        let client: Result<Client> = (&self.chain_config).try_into();
        let (daemon_operation, other_daemon_operations) = match (&batch.opid, client) {
            (Some(opid), Ok(client)) => {
                let this_op = client
                    .z_get_operation_status(vec![opid.as_str()])
                    .ok()
                    .and_then(|ops| ops.into_iter().next().flatten())
                    .and_then(|op| serde_json::to_value(&op).ok());

                let others = client
                    .z_get_operation_status(vec![])
                    .ok()
                    .map(|ops| {
                        ops.into_iter()
                            .flatten()
                            .filter(|op| Some(op.id.as_str()) != batch.opid.as_deref())
                            .filter_map(|op| serde_json::to_value(&op).ok())
                            .take(20)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                (this_op, others)
            }
            (None, Ok(client)) => {
                let others = client
                    .z_get_operation_status(vec![])
                    .ok()
                    .map(|ops| {
                        ops.into_iter()
                            .flatten()
                            .filter_map(|op| serde_json::to_value(&op).ok())
                            .take(20)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                (None, others)
            }
            (_, Err(e)) => {
                error!(error = ?e, "could not query daemon while building stuck-payout alert");
                (None, vec![])
            }
        };

        let msg = WebhookMessage::PayoutSendStuck {
            currency_address: self.chain_id.clone(),
            currency_name: self.currency_name.clone(),
            payment_batch_id: batch.batch_id,
            payment_opid: batch.opid.clone(),
            reason: reason.to_string(),
            daemon_operation,
            other_daemon_operations,
            members: batch
                .members
                .iter()
                .map(|m| PayoutStuckMember {
                    identity_address: m.identity_address.clone(),
                    block_hash: m.block_hash,
                    block_height: m.block_height,
                    reward: m.reward,
                })
                .collect(),
        };

        error!(
            batch_id = %batch.batch_id,
            opid = ?batch.opid,
            reason,
            n_members = batch.members.len(),
            "alerting admins: payout send is stuck and needs manual investigation"
        );

        self.webhooks.send(msg).await;
        self.notified_stuck_batches
            .lock()
            .unwrap()
            .insert(batch.batch_id);
    }

    async fn keep_creating_payouts(&self, subsys: &SubsystemHandle) -> Result<()> {
        while !subsys.is_shutdown_requested() {
            if let Err(e) = self.new_payout().await {
                error!(error = ?e, "Failed to create new payout");
            }

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {},
                _ = tokio::time::sleep(Duration::from_secs(self.config.check_interval_in_secs)) => {}
            }
        }

        Ok(())
    }

    async fn keep_sending_payments(&self, subsys: &SubsystemHandle) -> Result<()> {
        while !subsys.is_shutdown_requested() {
            if let Err(e) = self.send_unsent_payouts().await {
                error!(error = ?e, "Failed to send payment");
            }

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {},
                _ = tokio::time::sleep(Duration::from_secs(self.config.send_interval_in_secs)) => {}
            }
        }

        Ok(())
    }
}

pub fn prepare_payment<'a>(
    payout_members: &Vec<PayoutMember>,
) -> Result<Vec<SendCurrencyOutput<'a>>> {
    let mut payout_members_map: HashMap<Address, Amount> = HashMap::new();
    for member in payout_members.into_iter() {
        payout_members_map
            .entry(member.identity_address.clone())
            .and_modify(|sum| *sum += member.reward)
            .or_insert(member.reward);
    }

    // let payment_vouts = payout_members
    //     .iter()
    //     .map(|pm| {
    //         (
    //             pm.identity_address,
    //             pm.iter().fold(Amount::ZERO, |acc, pm| acc + pm.reward),
    //         )
    //     })
    //     .collect::<HashMap<&Address, Amount>>();

    debug!("payment_vouts {:#?}", payout_members_map);

    let outputs = payout_members_map
        .iter()
        .map(|(address, amount)| {
            SendCurrencyOutput::new(None, amount, &address.to_string(), None, None)
        })
        .collect::<Vec<_>>();

    Ok(outputs)
}

#[derive(Debug)]
enum WaitError {
    Failed(String),
    Timeout,
    Rpc(anyhow::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OperationOutcome {
    Pending,
    Success(Txid),
    Failed(String),
    Unknown,
}

pub(crate) fn classify_operation_status(
    status: Option<&ZOperationStatusResult>,
) -> OperationOutcome {
    let Some(opstatus) = status else {
        return OperationOutcome::Unknown;
    };

    match opstatus.status.as_str() {
        "queued" | "executing" => OperationOutcome::Pending,
        "success" => match opstatus.result.as_ref() {
            Some(result) => OperationOutcome::Success(result.txid),
            None => OperationOutcome::Failed("success status without txid".to_string()),
        },
        "failed" => {
            let msg = opstatus
                .error
                .as_ref()
                .map(|e| format!("{} (code {})", e.message, e.code))
                .unwrap_or_else(|| "failed".to_string());
            OperationOutcome::Failed(msg)
        }
        other => OperationOutcome::Failed(format!("unexpected status: {other}")),
    }
}

async fn wait_for_sendcurrency_finish(client: &Client, opid: &str) -> Result<Txid, WaitError> {
    // from https://buildmedia.readthedocs.org/media/pdf/zcash/english-docs/zcash.pdf
    // status can be one of queued, executing, failed or success.
    let deadline = Instant::now() + SENDCURRENCY_WAIT_TIMEOUT;

    loop {
        if Instant::now() >= deadline {
            return Err(WaitError::Timeout);
        }

        trace!("getting operation status: {}", &opid);
        let operation_status = client
            .z_get_operation_status(vec![opid])
            .map_err(|e| WaitError::Rpc(e.into()))?;
        trace!("got operation status: {:?}", &operation_status);

        match classify_operation_status(operation_status.first().and_then(|s| s.as_ref())) {
            OperationOutcome::Pending => {
                tokio::time::sleep(SENDCURRENCY_POLL_INTERVAL).await;
            }
            OperationOutcome::Success(txid) => return Ok(txid),
            OperationOutcome::Failed(msg) => return Err(WaitError::Failed(msg)),
            OperationOutcome::Unknown => {
                // The op may not have shown up yet immediately after sendcurrency.
                if Instant::now() + SENDCURRENCY_POLL_INTERVAL >= deadline {
                    return Err(WaitError::Timeout);
                }
                tokio::time::sleep(SENDCURRENCY_POLL_INTERVAL).await;
            }
        }
    }
}

#[async_trait::async_trait]
impl IntoSubsystem<anyhow::Error> for Service {
    async fn run(self, subsys: SubsystemHandle) -> Result<()> {
        tokio::try_join!(
            self.keep_creating_payouts(&subsys),
            self.keep_sending_payments(&subsys)
        )?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use vrsc_rpc::json::{
        ZOperationStatusResult, ZOperationStatusResultError, ZOperationStatusResultTxid,
    };

    fn opstatus(status: &str) -> ZOperationStatusResult {
        ZOperationStatusResult {
            id: "opid-test".to_string(),
            status: status.to_string(),
            creation_time: 0,
            result: None,
            error: None,
            execution_secs: None,
            method: "sendcurrency".to_string(),
            params: vec![],
        }
    }

    #[test]
    fn queued_and_executing_are_pending() {
        assert_eq!(
            classify_operation_status(Some(&opstatus("queued"))),
            OperationOutcome::Pending
        );
        assert_eq!(
            classify_operation_status(Some(&opstatus("executing"))),
            OperationOutcome::Pending
        );
    }

    #[test]
    fn missing_status_is_unknown() {
        assert_eq!(classify_operation_status(None), OperationOutcome::Unknown);
    }

    #[test]
    fn success_requires_txid() {
        let mut success = opstatus("success");
        assert!(matches!(
            classify_operation_status(Some(&success)),
            OperationOutcome::Failed(_)
        ));

        let txid =
            Txid::from_str("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                .unwrap();
        success.result = Some(ZOperationStatusResultTxid { txid });
        assert_eq!(
            classify_operation_status(Some(&success)),
            OperationOutcome::Success(txid)
        );
    }

    #[test]
    fn failed_includes_error_message() {
        let mut failed = opstatus("failed");
        failed.error = Some(ZOperationStatusResultError {
            code: -1,
            message: "insufficient funds".to_string(),
        });
        match classify_operation_status(Some(&failed)) {
            OperationOutcome::Failed(msg) => {
                assert!(msg.contains("insufficient funds"));
                assert!(msg.contains("-1"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn unexpected_status_is_failed() {
        assert!(matches!(
            classify_operation_status(Some(&opstatus("cancelled"))),
            OperationOutcome::Failed(_)
        ));
    }
}
