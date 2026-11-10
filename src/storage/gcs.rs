use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::ugs::{CallMode, UgsError};

pub const JOBS_CUSTOM_ID: &str = "kgrtp-mailbox-jobs";
pub const IDEMPOTENCY_CUSTOM_ID: &str = "kgrtp-mailbox-idem";
pub const MAIL_PREFIX: &str = "mail_";

const SCOPE: &str = "https://www.googleapis.com/auth/devstorage.read_write";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const METADATA_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";

#[derive(Debug, Clone)]
pub struct StoredItem {
    pub key: String,
    pub value: Option<Value>,
    pub write_lock: Option<String>,
}

#[derive(Clone)]
pub struct MailboxStore {
    inner: Arc<Inner>,
}

struct Inner {
    http: Client,
    bucket: String,
    token: Mutex<Option<CachedToken>>,
}

struct CachedToken {
    value: String,
    refresh_at: Instant,
}

impl MailboxStore {
    pub fn new(bucket: &str) -> Result<Self, String> {
        let http = Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|error| format!("storage http client: {error}"))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                bucket: bucket.to_owned(),
                token: Mutex::new(None),
            }),
        })
    }

    pub async fn get_protected(
        &self,
        player_id: &str,
        key: &str,
        _mode: CallMode,
    ) -> Result<Option<StoredItem>, UgsError> {
        let object = player_object(player_id, key)?;
        let mut item = self.read_object(&object).await?;
        if let Some(item) = item.as_mut() {
            item.key = key.to_owned();
        }
        Ok(item)
    }

    pub async fn list_protected_mail(
        &self,
        player_id: &str,
        _mode: CallMode,
    ) -> Result<Vec<StoredItem>, UgsError> {
        validate_segment(player_id)?;
        let names = self.list_names(&format!("players/{player_id}/")).await?;
        let objects = names
            .into_iter()
            .filter_map(|name| {
                let key = logical_key(&name)?;
                if key.starts_with(MAIL_PREFIX) {
                    Some((key, name))
                } else {
                    None
                }
            })
            .collect();
        self.read_many(objects).await
    }

    pub async fn set_protected(
        &self,
        player_id: &str,
        key: &str,
        value: &Value,
        write_lock: Option<&str>,
        _mode: CallMode,
    ) -> Result<Option<String>, UgsError> {
        let object = player_object(player_id, key)?;
        self.write_object(&object, value, write_lock).await
    }

    pub async fn delete_protected(
        &self,
        player_id: &str,
        key: &str,
        write_lock: &str,
        _mode: CallMode,
    ) -> Result<(), UgsError> {
        let object = player_object(player_id, key)?;
        self.delete_object(&object, write_lock).await
    }

    pub async fn get_private(
        &self,
        custom_id: &str,
        key: &str,
        _mode: CallMode,
    ) -> Result<Option<StoredItem>, UgsError> {
        let object = private_object(custom_id, key)?;
        let mut item = self.read_object(&object).await?;
        if let Some(item) = item.as_mut() {
            item.key = key.to_owned();
        }
        Ok(item)
    }

    pub async fn set_private(
        &self,
        custom_id: &str,
        key: &str,
        value: &Value,
        write_lock: Option<&str>,
        _mode: CallMode,
    ) -> Result<Option<String>, UgsError> {
        let object = private_object(custom_id, key)?;
        self.write_object(&object, value, write_lock).await
    }

    pub async fn list_private(
        &self,
        custom_id: &str,
        _mode: CallMode,
    ) -> Result<Vec<StoredItem>, UgsError> {
        let prefix = match custom_id {
            JOBS_CUSTOM_ID => "jobs/",
            IDEMPOTENCY_CUSTOM_ID => "idempotency/",
            _ => return Err(UgsError::BadRequest),
        };
        let names = self.list_names(prefix).await?;
        let objects = names
            .into_iter()
            .filter_map(|name| logical_key(&name).map(|key| (key, name)))
            .collect();
        self.read_many(objects).await
    }

    async fn read_many(&self, objects: Vec<(String, String)>) -> Result<Vec<StoredItem>, UgsError> {
        let mut items = Vec::with_capacity(objects.len());
        let mut index = 0;
        while index < objects.len() {
            let end = (index + 8).min(objects.len());
            let mut set = tokio::task::JoinSet::new();
            for (key, object) in objects[index..end].iter().cloned() {
                let store = self.clone();
                set.spawn(async move {
                    let found = store.read_object(&object).await?;
                    Ok::<_, UgsError>(found.map(|mut item| {
                        item.key = key;
                        item
                    }))
                });
            }
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(Ok(Some(item))) => items.push(item),
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => return Err(error),
                    Err(_) => return Err(UgsError::Unavailable),
                }
            }
            index = end;
        }
        items.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(items)
    }

    async fn read_object(&self, object: &str) -> Result<Option<StoredItem>, UgsError> {
        let url = format!(
            "https://storage.googleapis.com/storage/v1/b/{}/o/{}?alt=media",
            encode(&self.inner.bucket),
            encode(object)
        );
        let response = self
            .dispatch(|| self.inner.http.get(url.clone()))
            .await?;
        if response.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        expect_success(response.status)?;
        let generation = generation_header(&response.headers)?;
        let value = serde_json::from_slice(&response.body).map_err(|_| UgsError::BadResponse)?;
        Ok(Some(StoredItem {
            key: String::new(),
            value: Some(value),
            write_lock: Some(generation),
        }))
    }

    async fn write_object(
        &self,
        object: &str,
        value: &Value,
        write_lock: Option<&str>,
    ) -> Result<Option<String>, UgsError> {
        let body = serde_json::to_vec(value).map_err(|_| UgsError::BadResponse)?;
        let lock = match write_lock {
            Some(lock) => Some(require_generation(lock)?),
            None => None,
        };
        let custom_time = message_custom_time(object, value);
        let (boundary, payload) = multipart_upload(object, &body, custom_time.as_deref());
        let url = format!(
            "https://storage.googleapis.com/upload/storage/v1/b/{}/o?uploadType=multipart{}",
            encode(&self.inner.bucket),
            match &lock {
                Some(lock) => format!("&ifGenerationMatch={lock}"),
                None => String::new(),
            }
        );
        let content_type = format!("multipart/related; boundary={boundary}");
        let response = self
            .dispatch(|| {
                self.inner
                    .http
                    .post(url.clone())
                    .header("content-type", content_type.clone())
                    .body(payload.clone())
            })
            .await?;
        expect_success(response.status)?;
        let parsed: ObjectResource =
            serde_json::from_slice(&response.body).map_err(|_| UgsError::BadResponse)?;
        let generation = require_generation(&parsed.generation)?;
        Ok(Some(generation))
    }

    async fn delete_object(&self, object: &str, write_lock: &str) -> Result<(), UgsError> {
        let lock = require_generation(write_lock)?;
        let url = format!(
            "https://storage.googleapis.com/storage/v1/b/{}/o/{}",
            encode(&self.inner.bucket),
            encode(object)
        );
        let response = self
            .dispatch(|| {
                self.inner
                    .http
                    .delete(url.clone())
                    .header("x-goog-if-generation-match", lock.clone())
            })
            .await?;
        if response.status == StatusCode::NOT_FOUND {
            return Err(UgsError::NotFound);
        }
        expect_success(response.status)?;
        Ok(())
    }

    async fn list_names(&self, prefix: &str) -> Result<Vec<String>, UgsError> {
        let mut names = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..250 {
            let url = format!(
                "https://storage.googleapis.com/storage/v1/b/{}/o?prefix={}&maxResults=1000{}",
                encode(&self.inner.bucket),
                encode(prefix),
                page_token
                    .as_ref()
                    .map(|token| format!("&pageToken={}", encode(token)))
                    .unwrap_or_default()
            );
            let response = self
                .dispatch(|| self.inner.http.get(url.clone()))
                .await?;
            if response.status == StatusCode::NOT_FOUND {
                return Ok(names);
            }
            expect_success(response.status)?;
            let page: ListResponse =
                serde_json::from_slice(&response.body).map_err(|_| UgsError::BadResponse)?;
            names.extend(page.items.into_iter().map(|item| item.name));
            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => return Ok(names),
            }
        }
        Err(UgsError::BadResponse)
    }

    async fn dispatch<F>(&self, build: F) -> Result<RawResponse, UgsError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let token = self.access_token().await?;
        let response = send(build().bearer_auth(&token)).await?;
        if response.status == StatusCode::UNAUTHORIZED {
            self.clear_token().await;
            let token = self.access_token().await?;
            return send(build().bearer_auth(&token)).await;
        }
        Ok(response)
    }

    async fn access_token(&self) -> Result<String, UgsError> {
        {
            let guard = self.inner.token.lock().await;
            if let Some(cached) = guard.as_ref() {
                if cached.refresh_at > Instant::now() {
                    return Ok(cached.value.clone());
                }
            }
        }
        let fetched = fetch_token(&self.inner.http).await?;
        let mut guard = self.inner.token.lock().await;
        *guard = Some(CachedToken {
            value: fetched.value.clone(),
            refresh_at: Instant::now() + Duration::from_secs(fetched.expires_in.saturating_sub(60).max(30)),
        });
        Ok(fetched.value)
    }

    async fn clear_token(&self) {
        *self.inner.token.lock().await = None;
    }
}

struct RawResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

async fn send(request: reqwest::RequestBuilder) -> Result<RawResponse, UgsError> {
    let response = request.send().await.map_err(map_http)?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.map_err(map_http)?.to_vec();
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(UgsError::RateLimited {
            retry_after: retry_after(&headers),
        });
    }
    Ok(RawResponse {
        status,
        headers,
        body,
    })
}

fn expect_success(status: StatusCode) -> Result<(), UgsError> {
    if status.is_success() {
        return Ok(());
    }
    match status {
        StatusCode::NOT_FOUND => Err(UgsError::NotFound),
        StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => Err(UgsError::Conflict),
        StatusCode::BAD_REQUEST => Err(UgsError::BadRequest),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(UgsError::AuthRejected),
        StatusCode::REQUEST_TIMEOUT => Err(UgsError::Timeout),
        _ if status.is_server_error() => Err(UgsError::Unavailable),
        _ => Err(UgsError::BadResponse),
    }
}

fn generation_header(headers: &HeaderMap) -> Result<String, UgsError> {
    let value = headers
        .get("x-goog-generation")
        .ok_or(UgsError::BadResponse)?;
    let text = value.to_str().map_err(|_| UgsError::BadResponse)?;
    require_generation(text)
}

fn require_generation(value: &str) -> Result<String, UgsError> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(value.to_owned())
    } else {
        Err(UgsError::BadResponse)
    }
}

fn retry_after(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse().ok()
}

fn map_http(error: reqwest::Error) -> UgsError {
    if error.is_timeout() {
        UgsError::Timeout
    } else {
        UgsError::Unavailable
    }
}

struct FreshToken {
    value: String,
    expires_in: u64,
}

async fn fetch_token(http: &Client) -> Result<FreshToken, UgsError> {
    if let Some(token) = metadata_token(http).await {
        return Ok(token);
    }
    adc_token(http).await
}

async fn metadata_token(http: &Client) -> Option<FreshToken> {
    let response = http
        .get(METADATA_URL)
        .header("Metadata-Flavor", "Google")
        .timeout(Duration::from_millis(800))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: TokenResponse = response.json().await.ok()?;
    Some(FreshToken {
        value: body.access_token,
        expires_in: body.expires_in.unwrap_or(3600),
    })
}

async fn adc_token(http: &Client) -> Result<FreshToken, UgsError> {
    let path = adc_path().ok_or(UgsError::AuthRejected)?;
    let bytes = std::fs::read(&path).map_err(|_| UgsError::AuthRejected)?;
    let file: AdcFile = serde_json::from_slice(&bytes).map_err(|_| UgsError::AuthRejected)?;
    match file.kind.as_str() {
        "authorized_user" => refresh_user(http, &file).await,
        "service_account" => service_account_token(http, &file).await,
        _ => Err(UgsError::AuthRejected),
    }
}

fn adc_path() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        if !path.trim().is_empty() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    let home = std::env::var("APPDATA")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    let windows = std::path::Path::new(&home).join("gcloud").join("application_default_credentials.json");
    if windows.exists() {
        return Some(windows);
    }
    Some(
        std::path::Path::new(&home)
            .join(".config")
            .join("gcloud")
            .join("application_default_credentials.json"),
    )
}

async fn refresh_user(http: &Client, file: &AdcFile) -> Result<FreshToken, UgsError> {
    let client_id = file.client_id.as_deref().ok_or(UgsError::AuthRejected)?;
    let client_secret = file.client_secret.as_deref().ok_or(UgsError::AuthRejected)?;
    let refresh_token = file.refresh_token.as_deref().ok_or(UgsError::AuthRejected)?;
    let response = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(map_http)?;
    if !response.status().is_success() {
        return Err(UgsError::AuthRejected);
    }
    let body: TokenResponse = response.json().await.map_err(|_| UgsError::BadResponse)?;
    Ok(FreshToken {
        value: body.access_token,
        expires_in: body.expires_in.unwrap_or(3600),
    })
}

async fn service_account_token(http: &Client, file: &AdcFile) -> Result<FreshToken, UgsError> {
    let email = file.client_email.as_deref().ok_or(UgsError::AuthRejected)?;
    let key = file.private_key.as_deref().ok_or(UgsError::AuthRejected)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UgsError::BadResponse)?
        .as_secs() as i64;
    let claims = SaClaims {
        iss: email,
        scope: SCOPE,
        aud: TOKEN_URL,
        iat: now,
        exp: now + 3600,
    };
    let encoding = EncodingKey::from_rsa_pem(key.as_bytes()).map_err(|_| UgsError::AuthRejected)?;
    let assertion = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &encoding)
        .map_err(|_| UgsError::AuthRejected)?;
    let response = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", assertion.as_str()),
        ])
        .send()
        .await
        .map_err(map_http)?;
    if !response.status().is_success() {
        return Err(UgsError::AuthRejected);
    }
    let body: TokenResponse = response.json().await.map_err(|_| UgsError::BadResponse)?;
    Ok(FreshToken {
        value: body.access_token,
        expires_in: body.expires_in.unwrap_or(3600),
    })
}

fn message_custom_time(object: &str, value: &Value) -> Option<String> {
    if !object.starts_with("players/") {
        return None;
    }
    let text = value.get("expiresAt")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let parsed = DateTime::parse_from_rfc3339(text).ok()?;
    Some(parsed.with_timezone(&Utc).format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

fn multipart_upload(object: &str, body: &[u8], custom_time: Option<&str>) -> (String, Vec<u8>) {
    let boundary = format!("mailbox_{}", Uuid::new_v4().simple());
    let mut metadata = serde_json::json!({
        "name": object,
        "contentType": "application/json",
    });
    if let Some(custom_time) = custom_time {
        metadata["customTime"] = Value::String(custom_time.to_owned());
    }
    let metadata_text = metadata.to_string();
    let mut payload = Vec::new();
    payload.extend(
        format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata_text}\r\n--{boundary}\r\nContent-Type: application/json\r\n\r\n").as_bytes(),
    );
    payload.extend(body);
    payload.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
    (boundary, payload)
}

fn player_object(player_id: &str, key: &str) -> Result<String, UgsError> {
    validate_segment(player_id)?;
    validate_segment(key)?;
    Ok(format!("players/{player_id}/{key}.json"))
}

fn private_object(custom_id: &str, key: &str) -> Result<String, UgsError> {
    validate_segment(key)?;
    let folder = match custom_id {
        JOBS_CUSTOM_ID => "jobs",
        IDEMPOTENCY_CUSTOM_ID => "idempotency",
        _ => return Err(UgsError::BadRequest),
    };
    Ok(format!("{folder}/{key}.json"))
}

fn logical_key(object: &str) -> Option<String> {
    let file = object.rsplit('/').next()?;
    let key = file.strip_suffix(".json")?;
    if validate_segment(key).is_err() {
        return None;
    }
    Some(key.to_owned())
}

fn validate_segment(value: &str) -> Result<(), UgsError> {
    if value.is_empty()
        || value.len() > 256
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
    {
        return Err(UgsError::BadRequest);
    }
    Ok(())
}

fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct AdcFile {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    client_email: Option<String>,
    #[serde(default)]
    private_key: Option<String>,
}

#[derive(Serialize)]
struct SaClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListResponse {
    #[serde(default)]
    items: Vec<ListedObject>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct ListedObject {
    name: String,
}

#[derive(Deserialize)]
struct ObjectResource {
    generation: String,
}
