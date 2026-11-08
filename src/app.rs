use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::get;
use axum::Router;

use crate::admin::{admin_router, GoogleVerifier, SessionIssuer};
use crate::api::{api_router, health, normalize_statuses, not_found, request_context};
use crate::config::Config;
use crate::services::{BroadcastService, MailboxService, PlayerValidation};
use crate::state::AppState;
use crate::storage::CloudSave;
use crate::ugs::{PlayerAuth, UgsClient};
use crate::validation::MAX_BODY_BYTES;

pub fn build_state(config: Config) -> Result<AppState, String> {
    let ugs = UgsClient::new(&config)?;
    let http = ugs.http().clone();
    let cloud = CloudSave::new(ugs.clone());
    let players = PlayerAuth::new(ugs);
    let validation = PlayerValidation::new(players.clone());
    let mailbox = MailboxService::new(cloud.clone(), validation.clone());
    let broadcast = BroadcastService::new(cloud, players, mailbox.clone(), validation);
    Ok(AppState {
        sessions: SessionIssuer::new(&config.admin_session_secret, config.secure_cookies),
        google: GoogleVerifier::new(&config, http),
        config: Arc::new(config),
        mailbox,
        broadcast,
    })
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(api_router(state.clone()))
        .merge(admin_router(state.clone()))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn(normalize_statuses))
        .layer(middleware::from_fn(request_context))
        .with_state(state)
}
