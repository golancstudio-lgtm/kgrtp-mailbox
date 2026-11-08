use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::responses::{
    bearer_token, is_json_response, query_has_credential, scope_request, with_common_headers,
};
use crate::state::AppState;
use crate::models::AppError;
use crate::util::constant_time_eq;

pub async fn request_context(request: Request, next: Next) -> Response {
    let request_id = uuid::Uuid::new_v4();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let blocked = query_has_credential(request.uri().query());
    let started = Instant::now();
    let mut response = scope_request(request_id, async move {
        if blocked {
            return AppError::validation("Credentials must not be sent in the query string.")
                .into_response();
        }
        next.run(request).await
    })
    .await;
    let status = response.status().as_u16();
    with_common_headers(&mut response, request_id);
    tracing::info!(
        %request_id,
        %method,
        %path,
        status,
        duration_ms = started.elapsed().as_millis() as u64,
        "request"
    );
    response
}

pub async fn normalize_statuses(request: Request<Body>, next: Next) -> Response {
    let response = next.run(request).await;
    if is_json_response(&response) {
        return response;
    }
    match response.status() {
        StatusCode::METHOD_NOT_ALLOWED => {
            let allow = response.headers().get(header::ALLOW).cloned();
            let mut rewritten = AppError::method_not_allowed().into_response();
            if let Some(allow) = allow {
                rewritten.headers_mut().insert(header::ALLOW, allow);
            }
            rewritten
        }
        StatusCode::PAYLOAD_TOO_LARGE => AppError::payload_too_large().into_response(),
        _ => response,
    }
}

pub async fn require_bearer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let presented = bearer_token(request.headers()).unwrap_or("");
    if !constant_time_eq(
        presented.as_bytes(),
        state.config.mailbox_api_token.as_bytes(),
    ) {
        return AppError::unauthorized().into_response();
    }
    next.run(request).await
}

pub fn is_json_content_type(value: Option<&HeaderValue>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    text.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/json")
}
