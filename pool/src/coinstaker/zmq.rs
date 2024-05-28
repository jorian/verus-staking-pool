use std::str::FromStr;

use anyhow::Result;
use futures_util::stream::StreamExt;
use tokio::sync::mpsc;
use tracing::error;
use vrsc_rpc::bitcoin::BlockHash;

use super::coinstaker::CoinStakerMessage;

pub(super) async fn tmq_block_listen(
    port: u16,
    cx_tx: mpsc::Sender<CoinStakerMessage>,
) -> Result<()> {
    let mut socket = tmq::subscribe(&tmq::Context::new())
        .connect(&format!("tcp://127.0.0.1:{}", port))?
        .subscribe(b"hash")?;

    loop {
        if let Some(Ok(msg)) = socket.next().await {
            if let Some(hash) = msg.into_iter().nth(1) {
                let block_hash = hash
                    .iter()
                    .map(|byte| format!("{:02x}", *byte))
                    .collect::<Vec<_>>()
                    .join("");

                cx_tx
                    .send(CoinStakerMessage::Block(BlockHash::from_str(&block_hash)?))
                    .await?;
            } else {
                error!("not a valid message!");
            }
        } else {
            error!("no correct message received");
        }
    }
}
