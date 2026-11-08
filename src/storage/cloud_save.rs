use serde::Deserialize;
use serde_json::Value;

use crate::ugs::encode_path;
use crate::ugs::{AuthKind, CallMode, UgsClient, UgsError};

pub const JOBS_CUSTOM_ID: &str = "kgrtp-mailbox-jobs";
pub const IDEMPOTENCY_CUSTOM_ID: &str = "kgrtp-mailbox-idem";
pub const MAIL_PREFIX: &str = "mail_";

#[derive(Debug, Clone)]
pub struct StoredItem {
    pub key: String,
    pub value: Option<Value>,
    pub write_lock: Option<String>,
}

#[derive(Clone)]
pub struct CloudSave {
    client: UgsClient,
}

impl CloudSave {
    pub fn new(client: UgsClient) -> Self {
        Self { client }
    }

    pub async fn get_protected(
        &self,
        player_id: &str,
        key: &str,
        mode: CallMode,
    ) -> Result<Option<StoredItem>, UgsError> {
        let items = self
            .protected_page(player_id, Some(key), None, None, mode)
            .await?;
        Ok(items.into_iter().find(|item| item.key == key))
    }

    pub async fn list_protected_mail(
        &self,
        player_id: &str,
        mode: CallMode,
    ) -> Result<Vec<StoredItem>, UgsError> {
        self.list_protected(player_id, Some(MAIL_PREFIX), mode).await
    }

    async fn list_protected(
        &self,
        player_id: &str,
        prefix: Option<&str>,
        mode: CallMode,
    ) -> Result<Vec<StoredItem>, UgsError> {
        let mut collected = Vec::new();
        let mut after: Option<String> = None;
        let mut use_prefix = prefix.is_some();
        for page_index in 0..250 {
            let page = match self
                .protected_page(
                    player_id,
                    None,
                    after.as_deref(),
                    if use_prefix { prefix } else { None },
                    mode,
                )
                .await
            {
                Err(UgsError::BadRequest) if use_prefix && after.is_none() => {
                    tracing::warn!("cloud save prefix filter was rejected; filtering mail keys locally");
                    use_prefix = false;
                    continue;
                }
                Err(UgsError::NotFound) => return Ok(collected),
                other => other?,
            };
            if page.is_empty() {
                break;
            }
            let last_key = page.last().map(|item| item.key.clone());
            if after.as_ref() == last_key.as_ref() {
                return Err(UgsError::BadResponse);
            }
            let page_len = page.len();
            collected.extend(page.into_iter().filter(|item| {
                prefix
                    .map(|expected| item.key.starts_with(expected))
                    .unwrap_or(true)
            }));
            if page_len < 20 {
                break;
            }
            if page_index == 249 {
                return Err(UgsError::BadResponse);
            }
            after = last_key;
        }
        Ok(collected)
    }

    async fn protected_page(
        &self,
        player_id: &str,
        key: Option<&str>,
        after: Option<&str>,
        prefix: Option<&str>,
        mode: CallMode,
    ) -> Result<Vec<StoredItem>, UgsError> {
        let url = format!(
            "{}/players/{}/protected/items",
            self.client.cloud_save_root(),
            encode_path(player_id)
        );
        let key_owned = key.map(str::to_owned);
        let after_owned = after.map(str::to_owned);
        let prefix_owned = prefix.map(str::to_owned);
        let http = self.client.http().clone();
        let bytes = self
            .client
            .dispatch(
                mode,
                AuthKind::CloudSave,
                "get_protected_items",
                true,
                move || {
                    let mut request = http.get(url.clone());
                    if let Some(key) = &key_owned {
                        request = request.query(&[("keys", key.as_str())]);
                    }
                    if let Some(after) = &after_owned {
                        request = request.query(&[("after", after.as_str())]);
                    }
                    if let Some(prefix) = &prefix_owned {
                        request = request.query(&[("prefix", prefix.as_str())]);
                    }
                    request
                },
            )
            .await?;
        parse_items(&bytes)
    }

    pub async fn set_protected(
        &self,
        player_id: &str,
        key: &str,
        value: &Value,
        write_lock: Option<&str>,
        mode: CallMode,
    ) -> Result<Option<String>, UgsError> {
        let url = format!(
            "{}/players/{}/protected/items",
            self.client.cloud_save_root(),
            encode_path(player_id)
        );
        self.set_item(url, key, value, write_lock, mode, "set_protected_item")
            .await
    }

    pub async fn delete_protected(
        &self,
        player_id: &str,
        key: &str,
        write_lock: &str,
        mode: CallMode,
    ) -> Result<(), UgsError> {
        let url = format!(
            "{}/players/{}/protected/items/{}",
            self.client.cloud_save_root(),
            encode_path(player_id),
            encode_path(key)
        );
        let lock = write_lock.to_owned();
        let http = self.client.http().clone();
        self.client
            .dispatch(
                mode,
                AuthKind::CloudSave,
                "delete_protected_item",
                false,
                move || http.delete(url.clone()).query(&[("writeLock", lock.as_str())]),
            )
            .await?;
        Ok(())
    }

    pub async fn get_private(
        &self,
        custom_id: &str,
        key: &str,
        mode: CallMode,
    ) -> Result<Option<StoredItem>, UgsError> {
        let url = format!(
            "{}/custom/{}/private/items",
            self.client.cloud_save_root(),
            encode_path(custom_id)
        );
        let key_owned = key.to_owned();
        let http = self.client.http().clone();
        let bytes = match self
            .client
            .dispatch(
                mode,
                AuthKind::CloudSave,
                "get_private_custom_item",
                true,
                move || http.get(url.clone()).query(&[("keys", key_owned.as_str())]),
            )
            .await
        {
            Err(UgsError::NotFound) => return Ok(None),
            other => other?,
        };
        let items = parse_items(&bytes)?;
        Ok(items.into_iter().find(|item| item.key == key))
    }

    pub async fn set_private(
        &self,
        custom_id: &str,
        key: &str,
        value: &Value,
        write_lock: Option<&str>,
        mode: CallMode,
    ) -> Result<Option<String>, UgsError> {
        let url = format!(
            "{}/custom/{}/private/items",
            self.client.cloud_save_root(),
            encode_path(custom_id)
        );
        self.set_item(url, key, value, write_lock, mode, "set_private_custom_item")
            .await
    }

    pub async fn list_private(
        &self,
        custom_id: &str,
        mode: CallMode,
    ) -> Result<Vec<StoredItem>, UgsError> {
        let mut collected = Vec::new();
        let mut after: Option<String> = None;
        for page_index in 0..250 {
            let url = format!(
                "{}/custom/{}/private/items",
                self.client.cloud_save_root(),
                encode_path(custom_id)
            );
            let after_owned = after.clone();
            let http = self.client.http().clone();
            let bytes = match self
                .client
                .dispatch(
                    mode,
                    AuthKind::CloudSave,
                    "list_private_custom_items",
                    true,
                    move || {
                        let mut request = http.get(url.clone());
                        if let Some(after) = &after_owned {
                            request = request.query(&[("after", after.as_str())]);
                        }
                        request
                    },
                )
                .await
            {
                Err(UgsError::NotFound) => return Ok(collected),
                other => other?,
            };
            let page = parse_items(&bytes)?;
            if page.is_empty() {
                break;
            }
            let last_key = page.last().map(|item| item.key.clone());
            if after.as_ref() == last_key.as_ref() {
                return Err(UgsError::BadResponse);
            }
            let page_len = page.len();
            collected.extend(page);
            if page_len < 20 {
                break;
            }
            if page_index == 249 {
                return Err(UgsError::BadResponse);
            }
            after = last_key;
        }
        Ok(collected)
    }

    async fn set_item(
        &self,
        url: String,
        key: &str,
        value: &Value,
        write_lock: Option<&str>,
        mode: CallMode,
        operation: &'static str,
    ) -> Result<Option<String>, UgsError> {
        let mut body = serde_json::json!({
            "key": key,
            "value": value,
        });
        if let Some(lock) = write_lock {
            body["writeLock"] = Value::String(lock.to_owned());
        }
        let http = self.client.http().clone();
        let bytes = self
            .client
            .dispatch(mode, AuthKind::CloudSave, operation, false, move || {
                http.post(url.clone()).json(&body)
            })
            .await?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let parsed: WriteResponse = serde_json::from_slice(&bytes).unwrap_or(WriteResponse {
            write_lock: None,
        });
        Ok(parsed.write_lock)
    }
}

#[derive(Deserialize)]
struct WriteResponse {
    #[serde(default, rename = "writeLock")]
    write_lock: Option<String>,
}

#[derive(Deserialize)]
struct ItemsResponse {
    #[serde(default)]
    results: Vec<ItemBody>,
}

#[derive(Deserialize)]
struct ItemBody {
    key: String,
    #[serde(default)]
    value: Option<Value>,
    #[serde(default, rename = "writeLock")]
    write_lock: Option<String>,
}

fn parse_items(bytes: &[u8]) -> Result<Vec<StoredItem>, UgsError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let parsed: ItemsResponse = serde_json::from_slice(bytes).map_err(|_| UgsError::BadResponse)?;
    Ok(parsed
        .results
        .into_iter()
        .map(|item| StoredItem {
            key: item.key,
            value: item.value,
            write_lock: item.write_lock,
        })
        .collect())
}
