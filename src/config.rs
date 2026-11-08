use std::env;
use std::fmt;

#[derive(Clone)]
pub struct Config {
    pub port: u16,
    pub ugs_organization_id: String,
    pub ugs_project_id: String,
    pub ugs_environment_id: String,
    pub ugs_service_account_key_id: String,
    pub ugs_service_account_secret: String,
    pub mailbox_api_token: String,
    pub google_oauth_client_id: String,
    pub google_oauth_client_secret: String,
    pub admin_subjects: Vec<String>,
    pub admin_emails: Vec<String>,
    pub admin_session_secret: String,
    pub public_base_url: String,
    pub secure_cookies: bool,
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("port", &self.port)
            .field("ugs_project_id", &self.ugs_project_id)
            .field("ugs_environment_id", &self.ugs_environment_id)
            .field("public_base_url", &self.public_base_url)
            .field("admin_subjects", &self.admin_subjects.len())
            .field("admin_emails", &self.admin_emails.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct ConfigError {
    missing: Vec<String>,
    invalid: Vec<String>,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.missing.is_empty() {
            write!(
                formatter,
                "missing required environment variables: {}",
                self.missing.join(", ")
            )?;
        }
        if !self.invalid.is_empty() {
            if !self.missing.is_empty() {
                write!(formatter, "; ")?;
            }
            write!(
                formatter,
                "invalid environment variables: {}",
                self.invalid.join(", ")
            )?;
        }
        Ok(())
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut missing = Vec::new();
        let mut invalid = Vec::new();

        let port = match env::var("PORT") {
            Ok(value) if value.trim().is_empty() => {
                invalid.push("PORT (must be a TCP port)".to_owned());
                8080
            }
            Ok(value) => match value.trim().parse::<u16>() {
                Ok(port) if port > 0 => port,
                _ => {
                    invalid.push("PORT (must be a TCP port)".to_owned());
                    8080
                }
            },
            Err(_) => 8080,
        };

        let ugs_organization_id = required("UGS_ORGANIZATION_ID", &mut missing);
        let ugs_project_id = required("UGS_PROJECT_ID", &mut missing);
        let ugs_environment_id = required("UGS_ENVIRONMENT_ID", &mut missing);
        let ugs_service_account_key_id = required("UGS_SERVICE_ACCOUNT_KEY_ID", &mut missing);
        let ugs_service_account_secret = required("UGS_SERVICE_ACCOUNT_SECRET", &mut missing);
        let mailbox_api_token = required("MAILBOX_API_TOKEN", &mut missing);
        let google_oauth_client_id = required("GOOGLE_OAUTH_CLIENT_ID", &mut missing);
        let google_oauth_client_secret = required("GOOGLE_OAUTH_CLIENT_SECRET", &mut missing);
        let admin_session_secret = required("ADMIN_SESSION_SECRET", &mut missing);
        let public_base_url_raw = required("PUBLIC_BASE_URL", &mut missing);

        if !ugs_organization_id.is_empty() && !is_unity_id(&ugs_organization_id) {
            invalid.push("UGS_ORGANIZATION_ID (expected an identifier without spaces)".to_owned());
        }
        if !ugs_project_id.is_empty() && !is_unity_id(&ugs_project_id) {
            invalid.push("UGS_PROJECT_ID (expected an identifier without spaces)".to_owned());
        }
        if !ugs_environment_id.is_empty() && !is_unity_id(&ugs_environment_id) {
            invalid.push(
                "UGS_ENVIRONMENT_ID (expected an identifier without spaces)".to_owned(),
            );
        }
        if !ugs_service_account_key_id.is_empty()
            && !is_credential_token(&ugs_service_account_key_id)
        {
            invalid.push(
                "UGS_SERVICE_ACCOUNT_KEY_ID (must be printable ASCII without spaces)".to_owned(),
            );
        }
        if !ugs_service_account_secret.is_empty()
            && !is_credential_token(&ugs_service_account_secret)
        {
            invalid.push(
                "UGS_SERVICE_ACCOUNT_SECRET (must be printable ASCII without spaces)".to_owned(),
            );
        }
        if !mailbox_api_token.is_empty() && mailbox_api_token.chars().count() < 16 {
            invalid.push("MAILBOX_API_TOKEN (must be at least 16 characters)".to_owned());
        }
        if !mailbox_api_token.is_empty() && !is_credential_token(&mailbox_api_token) {
            invalid.push(
                "MAILBOX_API_TOKEN (must be printable ASCII without spaces)".to_owned(),
            );
        }
        if !google_oauth_client_id.is_empty() && google_oauth_client_id.chars().count() < 10 {
            invalid.push("GOOGLE_OAUTH_CLIENT_ID (too short)".to_owned());
        }
        if !google_oauth_client_secret.is_empty()
            && !is_credential_token(&google_oauth_client_secret)
        {
            invalid.push(
                "GOOGLE_OAUTH_CLIENT_SECRET (must be printable ASCII without spaces)".to_owned(),
            );
        }
        if !admin_session_secret.is_empty() && admin_session_secret.chars().count() < 32 {
            invalid.push("ADMIN_SESSION_SECRET (must be at least 32 characters)".to_owned());
        }

        let admin_subjects = optional_list("GOOGLE_ADMIN_SUBJECTS");
        let admin_emails = optional_list("GOOGLE_ADMIN_EMAILS")
            .into_iter()
            .map(|email| email.to_ascii_lowercase())
            .collect::<Vec<_>>();
        if env::var("GOOGLE_ADMIN_SUBJECTS")
            .ok()
            .is_some_and(|value| !value.trim().is_empty() && admin_subjects.is_empty())
        {
            invalid.push("GOOGLE_ADMIN_SUBJECTS (no usable entries)".to_owned());
        }
        if env::var("GOOGLE_ADMIN_EMAILS")
            .ok()
            .is_some_and(|value| !value.trim().is_empty() && admin_emails.is_empty())
        {
            invalid.push("GOOGLE_ADMIN_EMAILS (no usable entries)".to_owned());
        }
        if admin_subjects.is_empty() && admin_emails.is_empty() {
            invalid.push(
                "GOOGLE_ADMIN_SUBJECTS or GOOGLE_ADMIN_EMAILS (at least one allowlist entry is required)"
                    .to_owned(),
            );
        }
        if admin_subjects.iter().any(|subject| !is_subject(subject)) {
            invalid.push("GOOGLE_ADMIN_SUBJECTS (an entry is empty or contains spaces)".to_owned());
        }
        if admin_emails.iter().any(|email| !is_email(email)) {
            invalid.push("GOOGLE_ADMIN_EMAILS (an entry is not an email address)".to_owned());
        }

        let (public_base_url, secure_cookies) = if public_base_url_raw.is_empty() {
            (String::new(), true)
        } else {
            match normalize_base_url(&public_base_url_raw) {
                Ok(url) => {
                    let secure = url.starts_with("https://");
                    (url, secure)
                }
                Err(reason) => {
                    invalid.push(format!("PUBLIC_BASE_URL ({reason})"));
                    (String::new(), true)
                }
            }
        };

        if !missing.is_empty() || !invalid.is_empty() {
            return Err(ConfigError { missing, invalid });
        }

        Ok(Self {
            port,
            ugs_organization_id,
            ugs_project_id,
            ugs_environment_id,
            ugs_service_account_key_id,
            ugs_service_account_secret,
            mailbox_api_token,
            google_oauth_client_id,
            google_oauth_client_secret,
            admin_subjects,
            admin_emails,
            admin_session_secret,
            public_base_url,
            secure_cookies,
        })
    }

    pub fn google_secret_is_configured(&self) -> bool {
        !self.google_oauth_client_secret.is_empty()
    }
}

fn required(name: &str, missing: &mut Vec<String>) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            missing.push(name.to_owned());
            String::new()
        }
    }
}

fn optional_list(name: &str) -> Vec<String> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn is_unity_id(value: &str) -> bool {
    let length = value.len();
    (1..=64).contains(&length)
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
}

fn is_credential_token(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii() && !character.is_ascii_control() && !character.is_ascii_whitespace()
        })
}

fn is_subject(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | ':'))
}

fn is_email(value: &str) -> bool {
    let mut parts = value.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && value.chars().all(|character| !character.is_whitespace())
        }
        _ => false,
    }
}

fn normalize_base_url(value: &str) -> Result<String, &'static str> {
    let trimmed = value.trim().trim_end_matches('/').to_owned();
    let lower = trimmed.to_ascii_lowercase();
    let https = lower.starts_with("https://");
    let http = lower.starts_with("http://");
    if !https && !http {
        return Err("must be an absolute http or https URL");
    }
    if trimmed.contains(' ') || trimmed.contains('?') || trimmed.contains('#') || trimmed.contains('@')
    {
        return Err("must not contain whitespace, userinfo, a query, or a fragment");
    }
    let rest = if https { &trimmed[8..] } else { &trimmed[7..] };
    if rest.is_empty() || rest.contains('/') {
        return Err("must be an origin without a path");
    }
    if http {
        let host = rest.split(':').next().unwrap_or("");
        if host != "localhost" && host != "127.0.0.1" {
            return Err("http is only accepted for localhost");
        }
    }
    Ok(trimmed)
}
