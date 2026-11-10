mod admin;
mod api;
mod app;
mod config;
mod models;
mod services;
mod state;
mod storage;
mod ugs;
mod util;
mod validation;

use std::net::SocketAddr;
use std::time::Duration;

use tracing_subscriber::EnvFilter;

use crate::config::Config;

#[tokio::main]
async fn main() {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error}");
            std::process::exit(1);
        }
    };
    init_tracing();
    let state = match app::build_state(config) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("startup error: {error}");
            std::process::exit(1);
        }
    };
    tracing::info!(
        port = state.config.port,
        mailbox_bucket = %state.config.mailbox_bucket,
        project_id = %state.config.ugs_project_id,
        environment_id = %state.config.ugs_environment_id,
        organization_configured = !state.config.ugs_organization_id.is_empty(),
        google_client_secret_configured = state.config.google_secret_is_configured(),
        public_base_url = %state.config.public_base_url,
        "mailbox service starting"
    );
    state.broadcast.spawn();
    let address = SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, state.config.port));
    let listener = match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("failed to bind {address}: {error}");
            std::process::exit(1);
        }
    };
    tracing::info!(%address, "listening");
    let broadcast = state.broadcast.clone();
    let server = axum::serve(listener, app::router(state.clone())).with_graceful_shutdown(async move {
        shutdown_signal().await;
        broadcast.shutdown();
    });
    if let Err(error) = server.await {
        tracing::error!(%error, "server stopped");
    }
    let _ = tokio::time::timeout(Duration::from_secs(8), state.broadcast.stopped()).await;
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received");
}
