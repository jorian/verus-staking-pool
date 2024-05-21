pub mod constants;
mod handler;
mod routing;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::async_trait;
use tokio::net::TcpListener;
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemHandle};

use crate::{config::HttpConfig, controller::Controller};

use self::routing::router;

pub struct HttpService {
    pub state: Arc<Controller>,
    pub config: HttpConfig,
}

#[async_trait]
impl IntoSubsystem<anyhow::Error> for HttpService {
    async fn run(self, subsys: SubsystemHandle) -> Result<()> {
        let router = router(Arc::clone(&self.state));

        let socket = SocketAddr::new(self.config.host, self.config.port);
        let listener = TcpListener::bind(&socket).await?;
        let graceful_shutdown = |h: SubsystemHandle| async move { h.on_shutdown_requested().await };

        axum::serve(listener, router.into_make_service())
            .with_graceful_shutdown(graceful_shutdown(subsys))
            .await?;

        Ok(())
    }
}
