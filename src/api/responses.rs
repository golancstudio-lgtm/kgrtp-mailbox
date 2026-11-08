use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use uuid::Uuid;

use crate::models::{AppError, ErrorBody, ErrorObject};

pub const REQUEST_ID_HEADER: &str = "x-request-id";

tokio::task_local! {
    static REQUEST_ID: Uuid;
}

pub fn current_request_id() -> Uuid {
    REQUEST_ID.try_with(|id| *id).unwrap_or_else(|_| Uuid::nil())
}

pub async fn scope_request<F>(request_id: Uuid, future: F) -> Response
where
    F: std::future::Future<Output = Response>,
{
    REQUEST_ID.scope(request_id, future).await
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorBody {
                error: ErrorObject {
                    code: self.code,
                    message: self.message,
                    request_id: current_request_id(),
                },
            }),
        )
            .into_response();
        if let Some(seconds) = self.retry_after {
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

pub fn is_json_response(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().starts_with("application/json"))
        .unwrap_or(false)
}

pub fn with_common_headers(response: &mut Response, request_id: Uuid) {
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        headers.insert(REQUEST_ID_HEADER, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return None;
    }
    Some(token)
}

pub fn query_has_credential(query: Option<&str>) -> bool {
    let Some(query) = query else {
        return false;
    };
    let lower = query.to_ascii_lowercase();
    lower.contains("token=")
        || lower.contains("access_token=")
        || lower.contains("mailbox_api_token=")
        || lower.contains("authorization=")
}
