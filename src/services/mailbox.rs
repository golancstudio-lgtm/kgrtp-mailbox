use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::time;
use crate::models::{AppError, Message, Transition};
use crate::models::MessageInput;
use crate::services::player_validation::PlayerValidation;
use crate::storage::{CloudSave, StoredItem, IDEMPOTENCY_CUSTOM_ID, MAIL_PREFIX};
use crate::ugs::{map_transport, CallMode, UgsError};
use crate::util::sha256_hex;
use crate::validation::{validate_idempotency_key, validate_message_input, validate_player_id};

const IDEMPOTENCY_NAMESPACE: Uuid = Uuid::from_u128(0x8c2d4e106b7a4f3ca1d20e9f8b7c6d5e);

#[derive(Debug)]
pub struct SendOutcome {
    pub message: Message,
    pub created: bool,
}

#[derive(Debug)]
pub enum PreparedDelivery {
    Created,
    AlreadyPresent,
}

#[derive(Clone)]
pub struct MailboxService {
    cloud: CloudSave,
    players: PlayerValidation,
}

impl MailboxService {
    pub fn new(cloud: CloudSave, players: PlayerValidation) -> Self {
        Self { cloud, players }
    }

    pub async fn list_messages(&self, player_id: &str) -> Result<crate::models::MessageList, AppError> {
        validate_player_id(player_id)?;
        self.players.require_existing(player_id).await?;
        let items = self
            .cloud
            .list_protected_mail(player_id, CallMode::Interactive)
            .await
            .map_err(map_storage)?;
        let mut messages = Vec::with_capacity(items.len());
        for item in items {
            if !item.key.starts_with(MAIL_PREFIX) {
                continue;
            }
            messages.push(parse_stored(player_id, &item)?);
        }
        messages.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let total = messages.len();
        Ok(crate::models::MessageList {
            player_id: player_id.to_owned(),
            total,
            messages,
        })
    }

    pub async fn send(
        &self,
        player_id: &str,
        input: &MessageInput,
        raw_body: &[u8],
        idempotency_key: Option<&str>,
    ) -> Result<SendOutcome, AppError> {
        validate_player_id(player_id)?;
        validate_message_input(input)?;
        self.players.require_existing(player_id).await?;
        if let Some(key) = idempotency_key {
            validate_idempotency_key(key)?;
            return self
                .send_with_idempotency(player_id, input, raw_body, key)
                .await;
        }
        let message = Message::create(Uuid::new_v4(), player_id.to_owned(), input, Utc::now());
        self.write_message(&message, None, CallMode::Interactive).await?;
        Ok(SendOutcome {
            message,
            created: true,
        })
    }

    pub async fn mark_read(&self, player_id: &str, message_id: Uuid) -> Result<Message, AppError> {
        self.transition(player_id, message_id, TransitionKind::Read)
            .await
    }

    pub async fn mark_claim(&self, player_id: &str, message_id: Uuid) -> Result<Message, AppError> {
        self.transition(player_id, message_id, TransitionKind::Claim)
            .await
    }

    pub async fn delete_message(&self, player_id: &str, message_id: Uuid) -> Result<(), AppError> {
        validate_player_id(player_id)?;
        for attempt in 0..5 {
            let (message, lock) = self.load_message(player_id, message_id).await?;
            if message.has_unclaimed_attachments() {
                return Err(AppError::unclaimed_attachments());
            }
            match self
                .cloud
                .delete_protected(player_id, &mail_key(message_id), &lock, CallMode::Interactive)
                .await
            {
                Ok(()) => return Ok(()),
                Err(UgsError::Conflict) => {
                    sleep_conflict(attempt).await;
                    continue;
                }
                Err(UgsError::NotFound) => return Err(AppError::message_not_found()),
                Err(error) => return Err(map_storage(error)),
            }
        }
        Err(AppError::write_conflict())
    }

    /// Writes one prepared message. An existing key is left unchanged.
    /// Broadcast delivery uses this so a retry observes the original message.
    pub async fn deliver_prepared(
        &self,
        message: &Message,
        mode: CallMode,
    ) -> Result<PreparedDelivery, AppError> {
        validate_player_id(&message.recipient_id)?;
        let key = mail_key(message.id);
        let existing = self
            .cloud
            .get_protected(&message.recipient_id, &key, mode)
            .await
            .map_err(map_storage)?;
        if existing.is_some() {
            return Ok(PreparedDelivery::AlreadyPresent);
        }
        self.write_message(message, None, mode).await?;
        Ok(PreparedDelivery::Created)
    }

    async fn send_with_idempotency(
        &self,
        player_id: &str,
        input: &MessageInput,
        raw_body: &[u8],
        key: &str,
    ) -> Result<SendOutcome, AppError> {
        let message_id = Uuid::new_v5(
            &IDEMPOTENCY_NAMESPACE,
            format!("v1:{player_id}:{key}").as_bytes(),
        );
        let body_sha256 = sha256_hex(raw_body);
        let record_key = format!(
            "msg-{}",
            sha256_hex(format!("{player_id}\n{key}").as_bytes())
        );
        let now = Utc::now();
        if let Some(record) = self.read_idempotency(&record_key).await? {
            return self.replay(player_id, input, &record, &body_sha256).await;
        }
        let record = IdempotencyRecord {
            body_sha256: body_sha256.clone(),
            message_id,
            player_id: player_id.to_owned(),
            created_at: now,
        };
        self.write_idempotency(&record_key, &record, None).await?;
        let Some(confirmed) = self.read_idempotency(&record_key).await? else {
            return Err(AppError::bad_gateway());
        };
        self.replay(player_id, input, &confirmed, &body_sha256).await
    }

    async fn replay(
        &self,
        player_id: &str,
        input: &MessageInput,
        record: &IdempotencyRecord,
        body_sha256: &str,
    ) -> Result<SendOutcome, AppError> {
        if record.body_sha256 != body_sha256 || record.player_id != player_id {
            return Err(AppError::idempotency_conflict(
                "This Idempotency-Key was already used with a different request body.",
            ));
        }
        if let Some(existing) = self
            .load_optional(player_id, record.message_id)
            .await?
        {
            return Ok(SendOutcome {
                message: existing,
                created: false,
            });
        }
        let message = Message::create(
            record.message_id,
            player_id.to_owned(),
            input,
            record.created_at,
        );
        self.write_message(&message, None, CallMode::Interactive)
            .await?;
        Ok(SendOutcome {
            message,
            created: true,
        })
    }

    async fn transition(
        &self,
        player_id: &str,
        message_id: Uuid,
        kind: TransitionKind,
    ) -> Result<Message, AppError> {
        validate_player_id(player_id)?;
        for attempt in 0..5 {
            let (message, lock) = self.load_message(player_id, message_id).await?;
            let now = Utc::now();
            let transition = match kind {
                TransitionKind::Read => message.mark_read(now),
                TransitionKind::Claim => message.mark_claim(now).map_err(AppError::invalid_transition)?,
            };
            let Transition::Updated(updated) = transition else {
                return Ok(message);
            };
            let value = serde_json::to_value(&updated).map_err(|_| AppError::internal())?;
            match self
                .cloud
                .set_protected(player_id, &mail_key(message_id), &value, Some(&lock), CallMode::Interactive)
                .await
            {
                Ok(_) => return Ok(updated),
                Err(UgsError::Conflict) => {
                    sleep_conflict(attempt).await;
                    continue;
                }
                Err(error) => return Err(map_storage(error)),
            }
        }
        Err(AppError::write_conflict())
    }

    async fn load_message(&self, player_id: &str, message_id: Uuid) -> Result<(Message, String), AppError> {
        let item = self
            .load_item(player_id, message_id)
            .await?
            .ok_or_else(AppError::message_not_found)?;
        let lock = item
            .write_lock
            .clone()
            .ok_or_else(AppError::bad_gateway)?;
        let message = parse_stored(player_id, &item)?;
        Ok((message, lock))
    }

    async fn load_optional(&self, player_id: &str, message_id: Uuid) -> Result<Option<Message>, AppError> {
        let Some(item) = self.load_item(player_id, message_id).await? else {
            return Ok(None);
        };
        parse_stored(player_id, &item).map(Some)
    }

    async fn load_item(&self, player_id: &str, message_id: Uuid) -> Result<Option<StoredItem>, AppError> {
        self.cloud
            .get_protected(player_id, &mail_key(message_id), CallMode::Interactive)
            .await
            .map_err(map_storage)
    }

    async fn write_message(
        &self,
        message: &Message,
        write_lock: Option<&str>,
        mode: CallMode,
    ) -> Result<(), AppError> {
        let value = serde_json::to_value(message).map_err(|_| AppError::internal())?;
        self.cloud
            .set_protected(
                &message.recipient_id,
                &mail_key(message.id),
                &value,
                write_lock,
                mode,
            )
            .await
            .map_err(map_storage)?;
        Ok(())
    }

    async fn read_idempotency(&self, key: &str) -> Result<Option<IdempotencyRecord>, AppError> {
        let item = self
            .cloud
            .get_private(IDEMPOTENCY_CUSTOM_ID, key, CallMode::Interactive)
            .await
            .map_err(map_idempotency_storage)?;
        let Some(item) = item else {
            return Ok(None);
        };
        let Some(value) = item.value else {
            return Err(AppError::bad_gateway());
        };
        serde_json::from_value(value).map_err(|_| AppError::bad_gateway()).map(Some)
    }

    async fn write_idempotency(
        &self,
        key: &str,
        record: &IdempotencyRecord,
        write_lock: Option<&str>,
    ) -> Result<(), AppError> {
        let value = serde_json::to_value(record).map_err(|_| AppError::internal())?;
        self.cloud
            .set_private(IDEMPOTENCY_CUSTOM_ID, key, &value, write_lock, CallMode::Interactive)
            .await
            .map_err(map_idempotency_storage)?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum TransitionKind {
    Read,
    Claim,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdempotencyRecord {
    body_sha256: String,
    message_id: Uuid,
    player_id: String,
    #[serde(with = "time::required")]
    created_at: chrono::DateTime<Utc>,
}

pub fn mail_key(message_id: Uuid) -> String {
    format!("{MAIL_PREFIX}{message_id}")
}

fn parse_stored(player_id: &str, item: &StoredItem) -> Result<Message, AppError> {
    let Some(value) = item.value.clone() else {
        tracing::error!(player_id, key = %item.key, "stored mailbox entry has no value");
        return Err(AppError::unreadable_message());
    };
    let message: Message = match serde_json::from_value(value) {
        Ok(message) => message,
        Err(_) => {
            tracing::error!(player_id, key = %item.key, "stored mailbox entry is unreadable");
            return Err(AppError::unreadable_message());
        }
    };
    if message.schema_version != crate::models::message::SCHEMA_VERSION {
        tracing::error!(player_id, key = %item.key, "stored mailbox entry has an unsupported schema");
        return Err(AppError::unreadable_message());
    }
    let Some(key_id) = message_id_from_key(&item.key) else {
        tracing::error!(player_id, key = %item.key, "stored mailbox key is not a message id");
        return Err(AppError::unreadable_message());
    };
    if message.id != key_id || message.recipient_id != player_id {
        tracing::error!(player_id, key = %item.key, "stored mailbox entry does not match its key");
        return Err(AppError::unreadable_message());
    }
    Ok(message)
}

pub fn message_id_from_key(key: &str) -> Option<Uuid> {
    let raw = key.strip_prefix(MAIL_PREFIX)?;
    let id = Uuid::parse_str(raw).ok()?;
    if id.is_nil() {
        None
    } else {
        Some(id)
    }
}

fn map_storage(error: UgsError) -> AppError {
    match error {
        UgsError::NotFound => AppError::message_not_found(),
        other => map_transport(other),
    }
}

fn map_idempotency_storage(error: UgsError) -> AppError {
    match error {
        UgsError::StorageLimit => AppError::conflict(
            "STORAGE_LIMIT_REACHED",
            "The mailbox idempotency store has reached Unity's Cloud Save key limit.",
        ),
        other => map_transport(other),
    }
}

async fn sleep_conflict(attempt: u32) {
    let jitter = rand::random::<u64>() % 40;
    let millis = 25u64.saturating_mul(attempt as u64 + 1) + jitter;
    tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
}
