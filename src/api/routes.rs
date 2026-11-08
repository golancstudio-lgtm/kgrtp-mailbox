use axum::middleware;
use axum::routing::{delete, get, post, put};
use axum::Router;

use crate::api::handlers::{
    broadcast_status, delete_message, list_messages, mark_claim, mark_read, send_message,
    start_broadcast,
};
use crate::api::middleware::require_bearer;
use crate::state::AppState;

pub fn api_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/players/:player_id/messages",
            get(list_messages).post(send_message),
        )
        .route(
            "/api/v1/players/:player_id/messages/:message_id/read",
            put(mark_read),
        )
        .route(
            "/api/v1/players/:player_id/messages/:message_id/claim",
            put(mark_claim),
        )
        .route(
            "/api/v1/players/:player_id/messages/:message_id",
            delete(delete_message),
        )
        .route("/api/v1/messages/broadcast", post(start_broadcast))
        .route(
            "/api/v1/messages/broadcast/:broadcast_id",
            get(broadcast_status),
        )
        .route_layer(middleware::from_fn_with_state(state, require_bearer))
}
