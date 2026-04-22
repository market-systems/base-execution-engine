#![forbid(unsafe_code)]

//! Tracing, metrics, and health checks.

use anyhow::Context;
use config::MetricsConfig;
use metrics_exporter_prometheus::PrometheusBuilder;
use std::io;
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing_subscriber::EnvFilter;

static TRACING_INIT: OnceLock<()> = OnceLock::new();

pub struct ObservabilityRuntime {
    healthz_task: JoinHandle<io::Result<()>>,
    shutdown_healthz: Option<oneshot::Sender<()>>,
}

impl ObservabilityRuntime {
    pub fn init(config: &MetricsConfig) -> anyhow::Result<Self> {
        init_tracing(&config.log_level)?;

        if config.enabled {
            if let Some(addr) = config.prometheus_bind_addr {
                PrometheusBuilder::new()
                    .with_http_listener(addr)
                    .install_recorder()
                    .context("failed to install prometheus recorder")?;
                tracing::info!(%addr, "prometheus exporter listening");
            }
        }

        let (shutdown_healthz, healthz_shutdown_rx) = oneshot::channel();
        let bind_addr = config.healthz_bind_addr;
        let healthz_task =
            tokio::spawn(async move { run_healthz_server(bind_addr, healthz_shutdown_rx).await });
        tracing::info!(%bind_addr, "healthz endpoint listening");

        Ok(Self {
            healthz_task,
            shutdown_healthz: Some(shutdown_healthz),
        })
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(sender) = self.shutdown_healthz.take() {
            let _ = sender.send(());
        }

        self.healthz_task
            .await
            .context("healthz task join failed")?
            .context("healthz server failed")?;

        Ok(())
    }
}

fn init_tracing(log_level: &str) -> anyhow::Result<()> {
    if TRACING_INIT.get().is_some() {
        return Ok(());
    }

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(log_level))
        .context("failed to construct tracing filter")?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .json()
        .try_init()
        .ok();

    let _ = TRACING_INIT.set(());
    Ok(())
}

async fn run_healthz_server(
    bind_addr: std::net::SocketAddr,
    mut shutdown: oneshot::Receiver<()>,
) -> io::Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let (mut socket, _) = accepted?;
                tokio::spawn(async move {
                    let mut buffer = [0_u8; 1024];
                    let _ = socket.read(&mut buffer).await;
                    let response = b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";
                    let _ = socket.write_all(response).await;
                    let _ = socket.shutdown().await;
                });
            }
        }
    }

    Ok(())
}
