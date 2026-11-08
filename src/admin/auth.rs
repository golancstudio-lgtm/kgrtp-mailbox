use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::config::Config;
use crate::models::AppError;
use crate::util::constant_time_eq;

const CERTS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";

#[derive(Clone)]
pub struct GoogleVerifier {
    http: Client,
    client_id: String,
    subjects: Vec<String>,
    emails: Vec<String>,
    keys: Arc<RwLock<Option<CachedKeys>>>,
}

struct CachedKeys {
    by_kid: HashMap<String, DecodingKey>,
    fetched_at: Instant,
}

#[derive(Debug, Clone)]
pub struct GoogleIdentity {
    pub sub: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

impl GoogleVerifier {
    pub fn new(config: &Config, http: Client) -> Self {
        Self {
            http,
            client_id: config.google_oauth_client_id.clone(),
            subjects: config.admin_subjects.clone(),
            emails: config.admin_emails.clone(),
            keys: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn verify(&self, credential: &str) -> Result<GoogleIdentity, AppError> {
        if credential.len() < 20 || credential.len() > 8192 {
            return Err(AppError::google_failed());
        }
        let header = decode_header(credential).map_err(|_| AppError::google_failed())?;
        if header.alg != Algorithm::RS256 {
            return Err(AppError::google_failed());
        }
        let kid = header.kid.ok_or_else(AppError::google_failed)?;
        let key = self.key_for(&kid).await?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.client_id]);
        validation.set_issuer(&["accounts.google.com", "https://accounts.google.com"]);
        validation.leeway = 30;
        let data = decode::<GoogleClaims>(credential, &key, &validation)
            .map_err(|_| AppError::google_failed())?;
        if data.claims.sub.is_empty() || data.claims.sub.len() > 255 {
            return Err(AppError::google_failed());
        }
        Ok(GoogleIdentity {
            sub: data.claims.sub,
            email: data.claims.email.filter(|email| !email.is_empty()),
            email_verified: data.claims.email_verified,
            name: data.claims.name.filter(|name| !name.is_empty()),
        })
    }

    pub fn authorize(&self, identity: &GoogleIdentity) -> Result<(), AppError> {
        let subject_allowed = self.subjects.iter().any(|subject| {
            constant_time_eq(subject.as_bytes(), identity.sub.as_bytes())
        });
        let email_allowed = match &identity.email {
            Some(email) if identity.email_verified => {
                let normalized = email.to_ascii_lowercase();
                self.emails.iter().any(|allowed| allowed == &normalized)
            }
            _ => false,
        };
        if subject_allowed || email_allowed {
            Ok(())
        } else {
            Err(AppError::admin_denied())
        }
    }

    async fn key_for(&self, kid: &str) -> Result<DecodingKey, AppError> {
        if let Some(key) = self.cached_key(kid, false).await {
            return Ok(key);
        }
        self.refresh().await?;
        self.cached_key(kid, true)
            .await
            .ok_or_else(AppError::google_failed)
    }

    async fn cached_key(&self, kid: &str, ignore_age: bool) -> Option<DecodingKey> {
        let guard = self.keys.read().await;
        let cached = guard.as_ref()?;
        if !ignore_age && cached.fetched_at.elapsed() > Duration::from_secs(60 * 60) {
            return None;
        }
        cached.by_kid.get(kid).cloned()
    }

    async fn refresh(&self) -> Result<(), AppError> {
        let response = self
            .http
            .get(CERTS_URL)
            .send()
            .await
            .map_err(|_| AppError::google_failed())?;
        if !response.status().is_success() {
            return Err(AppError::google_failed());
        }
        let body: Jwks = response.json().await.map_err(|_| AppError::google_failed())?;
        let mut by_kid = HashMap::new();
        for key in body.keys {
            let (Some(kid), Some(modulus), Some(exponent)) = (key.kid, key.n, key.e) else {
                continue;
            };
            if key.kty.as_deref().is_some_and(|kind| kind != "RSA") {
                continue;
            }
            if let Ok(decoding) = DecodingKey::from_rsa_components(&modulus, &exponent) {
                by_kid.insert(kid, decoding);
            }
        }
        if by_kid.is_empty() {
            return Err(AppError::google_failed());
        }
        *self.keys.write().await = Some(CachedKeys {
            by_kid,
            fetched_at: Instant::now(),
        });
        Ok(())
    }
}

#[derive(Deserialize)]
struct GoogleClaims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default, deserialize_with = "deserialize_email_verified")]
    email_verified: bool,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: Option<String>,
    n: Option<String>,
    e: Option<String>,
    kty: Option<String>,
}

fn deserialize_email_verified<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Bool(verified) => Ok(verified),
        Value::String(text) => Ok(text.eq_ignore_ascii_case("true")),
        _ => Ok(false),
    }
}
