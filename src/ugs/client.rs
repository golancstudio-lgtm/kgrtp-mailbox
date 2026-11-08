use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::Utc;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};

use crate::config::Config;
use crate::ugs::authentication::basic_authorization_header;

const TOKEN_EXCHANGE_URL: &str = "https://services.api.unity.com/auth/v2/token-exchange";
const CLOUD_SAVE_SCOPES: &[&str] = &[
    "cloud_save.data.read",
    "cloud_save.data.upsert",
    "cloud_save.data.delete",
];

#[derive(Debug, Clone, Copy)]
pub enum CallMode {
    Interactive,
    Background,
}

#[derive(Debug, Clone, Copy)]
pub enum AuthKind {
    CloudSave,
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
    environment_id: String,
    basic_auth: HeaderValue,
    token: RwLock<Option<CachedToken>>,
    token_refresh: Mutex<()>,
}

struct CachedToken {
    value: String,
    refresh_at: Instant,
}

impl UgsClient {
    pub fn new(config: &Config) -> Result<Self, String> {
        let basic = basic_authorization_header(
            &config.ugs_service_account_key_id,
            &config.ugs_service_account_secret,
        );
        let basic_auth = HeaderValue::from_str(&basic)
            .map_err(|_| "UGS_SERVICE_ACCOUNT_KEY_ID or UGS_SERVICE_ACCOUNT_SECRET is not a valid header value".to_owned())?;
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
                environment_id: config.ugs_environment_id.clone(),
                basic_auth,
                token: RwLock::new(None),
                token_refresh: Mutex::new(()),
            }),
        })
    }

    pub fn http(&self) -> &Client {
        &self.inner.http
    }

    pub fn project_id(&self) -> &str {
        &self.inner.project_id
    }

    pub fn environment_id(&self) -> &str {
        &self.inner.environment_id
    }

    pub fn cloud_save_root(&self) -> String {
        format!(
            "https://services.api.unity.com/cloud-save/v1/data/projects/{}/environments/{}",
            self.project_id(),
            self.environment_id()
        )
    }

    pub async fn dispatch(
        &self,
        mode: CallMode,
        kind: AuthKind,
        operation: &'static str,
        idempotent: bool,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<Vec<u8>, UgsError> {
        let mut auth_retry = false;
        let mut attempt = 0u32;
        loop {
            let authorization = self.authorization(kind).await?;
            let started = Instant::now();
            let response = build().header(AUTHORIZATION, authorization).send().await;
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
                        tracing::debug!(operation, status = status.as_u16(), elapsed_ms = elapsed, "ugs request");
                        return Ok(bytes.to_vec());
                    }
                    if status == StatusCode::UNAUTHORIZED && matches!(kind, AuthKind::CloudSave) && !auth_retry {
                        auth_retry = true;
                        self.invalidate_token().await;
                        continue;
                    }
                    let body = String::from_utf8_lossy(&bytes);
                    let classified = classify(status, &body, retry_after);
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
                        elapsed_ms = elapsed,
                        "ugs request failed"
                    );
                    return Err(classified);
                }
            }
        }
    }

    async fn authorization(&self, kind: AuthKind) -> Result<HeaderValue, UgsError> {
        match kind {
            AuthKind::PlayerAuth => Ok(self.inner.basic_auth.clone()),
            AuthKind::CloudSave => {
                let token = self.cloud_save_token().await?;
                HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| UgsError::AuthRejected)
            }
        }
    }

    async fn cloud_save_token(&self) -> Result<String, UgsError> {
        if let Some(token) = self.fresh_token().await {
            return Ok(token);
        }
        let _guard = self.inner.token_refresh.lock().await;
        if let Some(token) = self.fresh_token().await {
            return Ok(token);
        }
        self.exchange_token().await
    }

    async fn fresh_token(&self) -> Option<String> {
        let guard = self.inner.token.read().await;
        guard.as_ref().and_then(|cached| {
            if Instant::now() < cached.refresh_at {
                Some(cached.value.clone())
            } else {
                None
            }
        })
    }

    async fn invalidate_token(&self) {
        *self.inner.token.write().await = None;
    }

    async fn exchange_token(&self) -> Result<String, UgsError> {
        let url = format!(
            "{TOKEN_EXCHANGE_URL}?projectId={}&environmentId={}",
            self.inner.project_id, self.inner.environment_id
        );
        let scoped = serde_json::json!({ "scopes": CLOUD_SAVE_SCOPES });
        let first = self.exchange_once(url.clone(), Some(scoped)).await;
        let token = match first {
            Ok(token) => token,
            Err(UgsError::BadRequest) => self.exchange_once(url, None).await?,
            Err(error) => return Err(error),
        };
        let refresh_at = jwt_expiry(&token)
            .map(|exp| {
                let now = Utc::now().timestamp();
                let seconds = (exp - now - 300).max(60) as u64;
                Instant::now() + Duration::from_secs(seconds)
            })
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(50 * 60));
        *self.inner.token.write().await = Some(CachedToken {
            value: token.clone(),
            refresh_at,
        });
        Ok(token)
    }

    async fn exchange_once(&self, url: String, body: Option<Value>) -> Result<String, UgsError> {
        let mut request = self
            .inner
            .http
            .request(Method::POST, url)
            .header(AUTHORIZATION, self.inner.basic_auth.clone());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                UgsError::Timeout
            } else {
                UgsError::Unavailable
            }
        })?;
        let status = response.status();
        let bytes = response.bytes().await.unwrap_or_default();
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            return Err(classify(status, &text, None));
        }
        let parsed: TokenResponse = serde_json::from_slice(&bytes).map_err(|_| UgsError::BadResponse)?;
        let token = parsed.access_token.or(parsed.access_token_snake).ok_or(UgsError::BadResponse)?;
        if token.is_empty() {
            return Err(UgsError::BadResponse);
        }
        Ok(token)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "access_token")]
    access_token_snake: Option<String>,
}

fn classify(status: StatusCode, body: &str, retry_after: Option<u64>) -> UgsError {
    let lower = body.to_ascii_lowercase();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => UgsError::AuthRejected,
        StatusCode::NOT_FOUND => UgsError::NotFound,
        StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => UgsError::Conflict,
        StatusCode::TOO_MANY_REQUESTS => UgsError::RateLimited { retry_after },
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => UgsError::Timeout,
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::INTERNAL_SERVER_ERROR => {
            UgsError::Unavailable
        }
        StatusCode::BAD_REQUEST => {
            let storage = (lower.contains("key") || lower.contains("item"))
                && (lower.contains("limit") || lower.contains("maximum") || lower.contains("exceed"));
            if storage {
                UgsError::StorageLimit
            } else if lower.contains("writelock") || lower.contains("write lock") || lower.contains("conflict") {
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

fn jwt_expiry(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.get("exp")?.as_i64()
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
        UgsError::Unavailable => crate::models::AppError::ugs_unavailable(
            "Unity Gaming Services is unavailable.",
        ),
        UgsError::BadResponse => crate::models::AppError::bad_gateway(),
    }
}
