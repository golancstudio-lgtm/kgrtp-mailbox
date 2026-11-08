use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use uuid::Uuid;

use crate::api::extract::{require_empty_body, JsonBody};
use crate::models::requests::MessageInput;
use crate::models::{AppError, BroadcastAccepted, BroadcastStatusKind, HealthResponse, MessageList};
use crate::state::AppState;
use crate::validation::validate_idempotency_key;

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "healthy" })
}

pub async fn list_messages(
    State(state): State<AppState>,
    Path(player_id): Path<String>,
) -> Result<Json<MessageList>, AppError> {
    let listed = state.mailbox.list_messages(&player_id).await?;
    Ok(Json(listed))
}

pub async fn send_message(
    State(state): State<AppState>,
    Path(player_id): Path<String>,
    headers: HeaderMap,
    body: JsonBody<MessageInput>,
) -> Result<Response, AppError> {
    let key = idempotency_key(&headers)?;
    let outcome = state
        .mailbox
        .send(&player_id, &body.value, &body.raw, key.as_deref())
        .await?;
    let location = format!(
        "{}/api/v1/players/{player_id}/messages/{}",
        state.config.public_base_url, outcome.message.id
    );
    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let mut response = (status, Json(outcome.message)).into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    Ok(response)
}

pub async fn mark_read(
    State(state): State<AppState>,
    Path((player_id, message_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<crate::models::Message>, AppError> {
    require_empty_body(&body)?;
    let message_id = parse_uuid(&message_id)?;
    let message = state.mailbox.mark_read(&player_id, message_id).await?;
    Ok(Json(message))
}

pub async fn mark_claim(
    State(state): State<AppState>,
    Path((player_id, message_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<crate::models::Message>, AppError> {
    require_empty_body(&body)?;
    let message_id = parse_uuid(&message_id)?;
    let message = state.mailbox.mark_claim(&player_id, message_id).await?;
    Ok(Json(message))
}

pub async fn delete_message(
    State(state): State<AppState>,
    Path((player_id, message_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    require_empty_body(&body)?;
    let message_id = parse_uuid(&message_id)?;
    state.mailbox.delete_message(&player_id, message_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn start_broadcast(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: JsonBody<MessageInput>,
) -> Result<Response, AppError> {
    let key = idempotency_key(&headers)?;
    let acceptance = state
        .broadcast
        .accept(&body.value, &body.raw, key.as_deref())
        .await?;
    if acceptance.created {
        let body = BroadcastAccepted {
            broadcast_id: acceptance.broadcast_id,
            status: BroadcastStatusKind::Pending,
        };
        return Ok((StatusCode::ACCEPTED, Json(body)).into_response());
    }
    let status = state.broadcast.status(acceptance.broadcast_id).await?;
    Ok((StatusCode::OK, Json(status)).into_response())
}

pub async fn broadcast_status(
    State(state): State<AppState>,
    Path(broadcast_id): Path<String>,
) -> Result<Json<crate::models::BroadcastStatus>, AppError> {
    let broadcast_id = parse_uuid(&broadcast_id)?;
    let status = state.broadcast.status(broadcast_id).await?;
    Ok(Json(status))
}

pub async fn not_found() -> AppError {
    AppError::not_found()
}

fn parse_uuid(value: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(value).map_err(|_| AppError::invalid_message_id())
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    let values: Vec<_> = headers.get_all("idempotency-key").iter().collect();
    if values.len() > 1 {
        return Err(AppError::validation("Send only one Idempotency-Key header."));
    }
    let Some(value) = values.first() else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| AppError::validation("Idempotency-Key must be visible ASCII."))?;
    validate_idempotency_key(text)?;
    Ok(Some(text.to_owned()))
}
