use serde::Deserialize;

use crate::ugs::client::{AuthKind, CallMode, UgsClient, UgsError};

#[derive(Debug, Clone, Deserialize)]
pub struct PlayerRecord {
    pub id: String,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone)]
pub struct PlayerPage {
    pub players: Vec<PlayerRecord>,
    pub next: Option<String>,
}

#[derive(Clone)]
pub struct PlayerAuth {
    client: UgsClient,
}

impl PlayerAuth {
    pub fn new(client: UgsClient) -> Self {
        Self { client }
    }

    pub async fn get_player(&self, player_id: &str) -> Result<PlayerRecord, UgsError> {
        let url = format!(
            "https://services.api.unity.com/player-identity/v1/projects/{}/users/{}",
            self.client.project_id(),
            encode_path(player_id)
        );
        let http = self.client.http().clone();
        let bytes = self
            .client
            .dispatch(CallMode::Interactive, AuthKind::PlayerAuth, "get_player", true, move || {
                http.get(url.clone())
            })
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| UgsError::BadResponse)
    }

    pub async fn list_players(
        &self,
        page: Option<&str>,
        mode: CallMode,
    ) -> Result<PlayerPage, UgsError> {
        let url = format!(
            "https://services.api.unity.com/player-identity/v1/projects/{}/users",
            self.client.project_id()
        );
        let page_owned = page.map(str::to_owned);
        let http = self.client.http().clone();
        let bytes = self
            .client
            .dispatch(mode, AuthKind::PlayerAuth, "list_players", true, move || {
                let mut request = http.get(url.clone()).query(&[("limit", "50")]);
                if let Some(page) = &page_owned {
                    request = request.query(&[("page", page.as_str())]);
                }
                request
            })
            .await?;
        let parsed: PlayerListResponse = serde_json::from_slice(&bytes).map_err(|_| UgsError::BadResponse)?;
        let next = parsed.next.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(normalize_page_token(trimmed))
            }
        });
        Ok(PlayerPage {
            players: parsed.results,
            next,
        })
    }
}

#[derive(Deserialize)]
struct PlayerListResponse {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    results: Vec<PlayerRecord>,
}

pub fn encode_path(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if unreserved {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn normalize_page_token(value: &str) -> String {
    let Ok(url) = reqwest::Url::parse(value) else {
        return value.to_owned();
    };
    url.query_pairs()
        .find(|(key, _)| key == "page")
        .map(|(_, page)| page.into_owned())
        .unwrap_or_else(|| value.to_owned())
}
