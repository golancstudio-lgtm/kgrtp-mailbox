use axum::http::{header, HeaderMap, HeaderValue};
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::util::constant_time_eq;

type HmacSha256 = Hmac<Sha256>;

const SESSION_SECONDS: i64 = 12 * 60 * 60;
const HOST_COOKIE: &str = "__Host-kgrtp_admin_session";
const COOKIE: &str = "kgrtp_admin_session";

#[derive(Clone)]
pub struct SessionIssuer {
    secret: Vec<u8>,
    secure: bool,
}

#[derive(Debug, Clone)]
pub struct AdminSession {
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub csrf: String,
}

#[derive(Serialize, Deserialize)]
struct SessionClaims {
    sub: String,
    email: Option<String>,
    name: Option<String>,
    exp: i64,
    csrf: String,
}

impl SessionIssuer {
    pub fn new(secret: &str, secure: bool) -> Self {
        Self {
            secret: secret.as_bytes().to_vec(),
            secure,
        }
    }

    pub fn issue(&self, sub: &str, email: Option<String>, name: Option<String>) -> Option<(String, AdminSession)> {
        let mut random = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let csrf = b64(&random);
        let claims = SessionClaims {
            sub: sub.to_owned(),
            email,
            name,
            exp: Utc::now().timestamp() + SESSION_SECONDS,
            csrf: csrf.clone(),
        };
        let payload = serde_json::to_vec(&claims).ok()?;
        let signature = self.sign(&payload)?;
        let token = format!("{}.{}", b64(&payload), b64(&signature));
        let session = AdminSession {
            sub: claims.sub,
            email: claims.email,
            name: claims.name,
            csrf,
        };
        Some((self.set_cookie(&token, SESSION_SECONDS), session))
    }

    pub fn read(&self, headers: &HeaderMap) -> Option<AdminSession> {
        let token = read_cookie(headers, HOST_COOKIE).or_else(|| read_cookie(headers, COOKIE))?;
        let (payload_b64, signature_b64) = token.split_once('.')?;
        let payload = b64_decode(payload_b64)?;
        let signature = b64_decode(signature_b64)?;
        let expected = self.sign(&payload)?;
        if !constant_time_eq(&signature, &expected) {
            return None;
        }
        let claims: SessionClaims = serde_json::from_slice(&payload).ok()?;
        if claims.exp < Utc::now().timestamp() || claims.sub.is_empty() || claims.csrf.is_empty() {
            return None;
        }
        Some(AdminSession {
            sub: claims.sub,
            email: claims.email,
            name: claims.name,
            csrf: claims.csrf,
        })
    }

    pub fn clear_cookie(&self) -> HeaderValue {
        let name = if self.secure { HOST_COOKIE } else { COOKIE };
        let secure = if self.secure { "; Secure" } else { "" };
        HeaderValue::from_str(&format!(
            "{name}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}"
        ))
        .unwrap_or_else(|_| HeaderValue::from_static("kgrtp_admin_session=; Max-Age=0; Path=/"))
    }

    fn set_cookie(&self, token: &str, max_age: i64) -> String {
        let name = if self.secure { HOST_COOKIE } else { COOKIE };
        let secure = if self.secure { "; Secure" } else { "" };
        format!("{name}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{secure}")
    }

    fn sign(&self, payload: &[u8]) -> Option<Vec<u8>> {
        let mut mac = HmacSha256::new_from_slice(&self.secret).ok()?;
        mac.update(payload);
        Some(mac.finalize().into_bytes().to_vec())
    }
}

impl AdminSession {
    pub fn display_name(&self) -> String {
        self.email
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_else(|| self.sub.clone())
    }
}

fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    let prefix = format!("{name}=");
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(&prefix) {
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn b64_decode(value: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()
}
