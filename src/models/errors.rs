use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BroadcastAccepted {
    pub broadcast_id: Uuid,
    pub status: BroadcastStatusKind,
}

pub use crate::models::broadcast::BroadcastStatusKind;

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub error: ErrorObject,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorObject {
    pub code: &'static str,
    pub message: String,
    pub request_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct AppError {
    pub status: axum::http::StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retry_after: Option<u64>,
}

impl AppError {
    pub fn new(
        status: axum::http::StatusCode,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    pub fn with_retry(mut self, seconds: u64) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    pub fn unauthorized() -> Self {
        Self::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "INVALID_TOKEN",
            "Missing or invalid bearer token.",
        )
    }

    pub fn admin_denied() -> Self {
        Self::new(
            axum::http::StatusCode::FORBIDDEN,
            "ADMIN_ACCESS_DENIED",
            "This Google account is not allowed to administer the mailbox.",
        )
    }

    pub fn google_failed() -> Self {
        Self::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "GOOGLE_AUTH_FAILED",
            "Google sign-in could not be verified.",
        )
    }

    pub fn invalid_player_id() -> Self {
        Self::new(
            axum::http::StatusCode::BAD_REQUEST,
            "INVALID_PLAYER_ID",
            "playerId must be 1-128 characters of letters, digits, underscores, or hyphens.",
        )
    }

    pub fn invalid_message_id() -> Self {
        Self::new(
            axum::http::StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "messageId must be a UUID.",
        )
    }

    pub fn player_not_found() -> Self {
        Self::new(
            axum::http::StatusCode::NOT_FOUND,
            "PLAYER_NOT_FOUND",
            "The player does not exist.",
        )
    }

    pub fn message_not_found() -> Self {
        Self::new(
            axum::http::StatusCode::NOT_FOUND,
            "MESSAGE_NOT_FOUND",
            "The requested message does not exist.",
        )
    }

    pub fn broadcast_not_found() -> Self {
        Self::new(
            axum::http::StatusCode::NOT_FOUND,
            "BROADCAST_NOT_FOUND",
            "The broadcast does not exist.",
        )
    }

    pub fn invalid_json(message: impl Into<String>) -> Self {
        Self::new(
            axum::http::StatusCode::BAD_REQUEST,
            "INVALID_JSON",
            message,
        )
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            message,
        )
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(axum::http::StatusCode::CONFLICT, code, message)
    }

    pub fn unclaimed_attachments() -> Self {
        Self::conflict(
            "UNCLAIMED_ATTACHMENTS",
            "Messages with unclaimed attachments cannot be deleted.",
        )
    }

    pub fn invalid_transition(message: impl Into<String>) -> Self {
        Self::conflict("INVALID_STATE_TRANSITION", message)
    }

    pub fn write_conflict() -> Self {
        Self::conflict(
            "WRITE_CONFLICT",
            "The message was updated concurrently. Retry the request.",
        )
    }

    pub fn idempotency_conflict(message: impl Into<String>) -> Self {
        Self::conflict("IDEMPOTENCY_CONFLICT", message)
    }

    pub fn storage_limit() -> Self {
        Self::conflict(
            "STORAGE_LIMIT_REACHED",
            "Protected Cloud Save storage is full for this player. No messages were discarded.",
        )
    }

    pub fn rate_limited(retry_after: Option<u64>) -> Self {
        let error = Self::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Unity Gaming Services rate limited the request.",
        );
        match retry_after {
            Some(seconds) => error.with_retry(seconds),
            None => error,
        }
    }

    pub fn ugs_timeout() -> Self {
        Self::new(
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            "UGS_TIMEOUT",
            "Unity Gaming Services did not respond in time.",
        )
    }

    pub fn ugs_unavailable(message: impl Into<String>) -> Self {
        Self::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "UGS_UNAVAILABLE",
            message,
        )
    }

    pub fn ugs_auth() -> Self {
        Self::new(
            axum::http::StatusCode::BAD_GATEWAY,
            "UGS_UNAVAILABLE",
            "Unity rejected the service account. Check the Cloud Save and Player Authentication permissions.",
        )
    }

    pub fn bad_gateway() -> Self {
        Self::new(
            axum::http::StatusCode::BAD_GATEWAY,
            "UGS_UNAVAILABLE",
            "Unity Gaming Services returned an unexpected response.",
        )
    }

    pub fn unreadable_message() -> Self {
        Self::new(
            axum::http::StatusCode::BAD_GATEWAY,
            "UGS_UNAVAILABLE",
            "A stored mailbox entry could not be read and was left unchanged.",
        )
    }

    pub fn internal() -> Self {
        Self::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "The mailbox service could not complete the request.",
        )
    }

    pub fn payload_too_large() -> Self {
        Self::new(
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "CONTENT_TOO_LARGE",
            "JSON request bodies are limited to 64 KiB.",
        )
    }

    pub fn unsupported_media() -> Self {
        Self::new(
            axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_MEDIA_TYPE",
            "Content-Type must be application/json.",
        )
    }

    pub fn method_not_allowed() -> Self {
        Self::new(
            axum::http::StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
            "The HTTP method is not allowed for this resource.",
        )
    }

    pub fn not_found() -> Self {
        Self::new(
            axum::http::StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "The requested resource does not exist.",
        )
    }

    pub fn csrf() -> Self {
        Self::new(
            axum::http::StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "The CSRF token is missing or invalid.",
        )
    }

    pub fn session_required() -> Self {
        Self::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "GOOGLE_AUTH_FAILED",
            "Sign in with an authorized Google account.",
        )
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}
