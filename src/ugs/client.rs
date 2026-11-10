use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::Value;

use crate::config::Config;
use crate::ugs::authentication::basic_authorization_header;

#[derive(Debug, Clone, Copy)]
pub enum CallMode {
    Interactive,
    Background,
}

#[derive(Debug, Clone, Copy)]
pub enum AuthKind {
    PlayerAuth,
}

#[derive(Debug, thiserror::Error)]
pub enum UgsError {
    #[error("unity service account was rejected")]
    AuthRejected,
    #[error("unity resource was not found")]
    NotFound,
    #[error("unity write conflict")]
    Conflict,
    #[error("unity storage limit")]
    StorageLimit,
    #[error("unity rejected the request")]
    BadRequest,
    #[error("unity rate limited the request")]
    RateLimited { retry_after: Option<u64> },
    #[error("unity request timed out")]
    Timeout,
    #[error("unity was unavailable")]
    Unavailable,
    #[error("unexpected unity response")]
    BadResponse,
}

#[derive(Clone)]
pub struct UgsClient {
    inner: Arc<UgsInner>,
}

struct UgsInner {
    http: Client,
    project_id: String,
    basic_auth: HeaderValue,
    key_id: String,
    secret: String,
}

impl UgsClient {
    pub fn new(config: &Config) -> Result<Self, String> {
        let basic = basic_authorization_header(
            &config.ugs_service_account_key_id,
            &config.ugs_service_account_secret,
        );
        let basic_auth = HeaderValue::from_str(&basic).map_err(|_| {
            "UGS_SERVICE_ACCOUNT_KEY_ID or UGS_SERVICE_ACCOUNT_SECRET is not a valid header value"
                .to_owned()
        })?;
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .user_agent("kgrtp-mailbox/1.0")
            .https_only(true)
            .pool_max_idle_per_host(8)
            .build()
            .map_err(|_| "failed to build the Unity HTTP client".to_owned())?;
        Ok(Self {
            inner: Arc::new(UgsInner {
                http,
                project_id: config.ugs_project_id.clone(),
                basic_auth,
                key_id: config.ugs_service_account_key_id.clone(),
                secret: config.ugs_service_account_secret.clone(),
            }),
        })
    }

    pub fn http(&self) -> &Client {
        &self.inner.http
    }

    pub fn project_id(&self) -> &str {
        &self.inner.project_id
    }

    pub async fn dispatch(
        &self,
        mode: CallMode,
        kind: AuthKind,
        operation: &'static str,
        idempotent: bool,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<Vec<u8>, UgsError> {
        let mut attempt = 0u32;
        loop {
            let started = Instant::now();
            let response = build()
                .header(AUTHORIZATION, self.authorization(kind))
                .send()
                .await;
            match response {
                Err(error) if error.is_timeout() => {
                    if idempotent && attempt < 2 {
                        attempt += 1;
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    tracing::warn!(operation, "ugs request timed out");
                    return Err(UgsError::Timeout);
                }
                Err(error) if error.is_connect() || error.is_request() => {
                    if attempt < 2 {
                        attempt += 1;
                        tokio::time::sleep(backoff(attempt)).await;
                        continue;
                    }
                    tracing::warn!(operation, "ugs request could not connect");
                    return Err(UgsError::Unavailable);
                }
                Err(_) => return Err(UgsError::Unavailable),
                Ok(response) => {
                    let status = response.status();
                    let retry_after = retry_after_header(response.headers());
                    let bytes = response.bytes().await.unwrap_or_default();
                    let elapsed = started.elapsed().as_millis() as u64;
                    if status.is_success() {
                        tracing::debug!(
                            operation,
                            status = status.as_u16(),
                            elapsed_ms = elapsed,
                            "ugs request"
                        );
                        return Ok(bytes.to_vec());
                    }
                    let body = String::from_utf8_lossy(&bytes);
                    let classified = classify(status, &body, retry_after);
                    let problem = unity_problem(&body, &self.inner.key_id, &self.inner.secret);
                    let retryable = matches!(
                        classified,
                        UgsError::RateLimited { .. } | UgsError::Unavailable
                    );
                    if retryable && attempt < 3 {
                        let wait = match &classified {
                            UgsError::RateLimited { retry_after } => retry_after
                                .map(Duration::from_secs)
                                .unwrap_or_else(|| backoff(attempt)),
                            _ => backoff(attempt),
                        };
                        let cap = match mode {
                            CallMode::Interactive => Duration::from_secs(5),
                            CallMode::Background => Duration::from_secs(120),
                        };
                        if wait <= cap {
                            attempt += 1;
                            tokio::time::sleep(wait).await;
                            continue;
                        }
                    }
                    tracing::warn!(
                        operation,
                        status = status.as_u16(),
                        unity_code = problem.code.as_deref().unwrap_or(""),
                        unity_title = problem.title.as_deref().unwrap_or(""),
                        unity_detail = problem.detail.as_deref().unwrap_or(""),
                        elapsed_ms = elapsed,
                        "ugs request failed"
                    );
                    return Err(classified);
                }
            }
        }
    }

    /// Cloud Save Data Admin and Player Authentication Admin both use HTTP Basic.
    /// The stateless Bearer token is for the player Cloud Save host, not these admin routes.
    fn authorization(&self, _kind: AuthKind) -> HeaderValue {
        self.inner.basic_auth.clone()
    }
}

struct UnityProblem {
    code: Option<String>,
    title: Option<String>,
    detail: Option<String>,
}

fn unity_problem(body: &str, key_id: &str, secret: &str) -> UnityProblem {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return UnityProblem {
            code: None,
            title: None,
            detail: None,
        };
    };
    UnityProblem {
        code: value.get("code").and_then(json_scalar).and_then(|text| sanitize(&text, key_id, secret)),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .and_then(|text| sanitize(text, key_id, secret)),
        detail: value
            .get("detail")
            .and_then(Value::as_str)
            .and_then(|text| sanitize(text, key_id, secret)),
    }
}

fn json_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn sanitize(value: &str, key_id: &str, secret: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("bearer")
        || lower.contains("basic")
        || lower.contains("authorization")
        || (!key_id.is_empty() && value.contains(key_id))
        || (!secret.is_empty() && value.contains(secret))
    {
        return None;
    }
    let mut characters = value.chars();
    let short: String = characters.by_ref().take(180).collect();
    if characters.next().is_some() {
        Some(format!("{short}..."))
    } else {
        Some(short)
    }
}

fn classify(status: StatusCode, body: &str, retry_after: Option<u64>) -> UgsError {
    let lower = body.to_ascii_lowercase();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => UgsError::AuthRejected,
        StatusCode::NOT_FOUND => UgsError::NotFound,
        StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => UgsError::Conflict,
        StatusCode::TOO_MANY_REQUESTS => UgsError::RateLimited { retry_after },
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => UgsError::Timeout,
        StatusCode::BAD_GATEWAY
        | StatusCode::SERVICE_UNAVAILABLE
        | StatusCode::INTERNAL_SERVER_ERROR => UgsError::Unavailable,
        StatusCode::BAD_REQUEST => {
            let storage = (lower.contains("key") || lower.contains("item"))
                && (lower.contains("limit")
                    || lower.contains("maximum")
                    || lower.contains("exceed"));
            if storage {
                UgsError::StorageLimit
            } else if lower.contains("writelock")
                || lower.contains("write lock")
                || lower.contains("conflict")
            {
                UgsError::Conflict
            } else {
                UgsError::BadRequest
            }
        }
        _ => UgsError::BadResponse,
    }
}

fn retry_after_header(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse::<u64>().ok()
}

fn backoff(attempt: u32) -> Duration {
    let base = 200u64.saturating_mul(1u64 << attempt.min(4));
    let jitter = rand::random::<u64>() % 150;
    Duration::from_millis(base + jitter)
}

pub fn map_transport(error: UgsError) -> crate::models::AppError {
    match error {
        UgsError::AuthRejected => crate::models::AppError::ugs_auth(),
        UgsError::NotFound => crate::models::AppError::bad_gateway(),
        UgsError::Conflict => crate::models::AppError::write_conflict(),
        UgsError::StorageLimit => crate::models::AppError::storage_limit(),
        UgsError::BadRequest => crate::models::AppError::bad_gateway(),
        UgsError::RateLimited { retry_after } => crate::models::AppError::rate_limited(retry_after),
        UgsError::Timeout => crate::models::AppError::ugs_timeout(),
        UgsError::Unavailable => {
            crate::models::AppError::ugs_unavailable("Unity Gaming Services is unavailable.")
        }
        UgsError::BadResponse => crate::models::AppError::bad_gateway(),
    }
}
