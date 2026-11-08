use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::models::{AppError, MessageInput, Sender, SenderType};

pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_ATTACHMENTS: usize = 20;
pub const MAX_SUBJECT_CHARS: usize = 120;
pub const MAX_BODY_CHARS: usize = 5_000;
pub const MAX_DISPLAY_NAME_CHARS: usize = 120;
pub const MAX_METADATA_BYTES: usize = 2 * 1024;
pub const MAX_METADATA_DEPTH: usize = 8;
pub const MAX_IDEMPOTENCY_KEY_CHARS: usize = 128;

const RESERVED_METADATA_KEYS: &[&str] = &[
    "schemaVersion",
    "id",
    "sender",
    "recipientId",
    "category",
    "subject",
    "body",
    "state",
    "createdAt",
    "readAt",
    "claimedAt",
    "expiresAt",
    "attachments",
    "metadata",
];

pub fn validate_player_id(value: &str) -> Result<(), AppError> {
    if is_safe_id(value) {
        Ok(())
    } else {
        Err(AppError::invalid_player_id())
    }
}

pub fn is_safe_id(value: &str) -> bool {
    let length = value.chars().count();
    (1..=128).contains(&length)
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-')
}

pub fn validate_idempotency_key(value: &str) -> Result<(), AppError> {
    let length = value.chars().count();
    if length == 0 || length > MAX_IDEMPOTENCY_KEY_CHARS {
        return Err(AppError::validation(
            "Idempotency-Key must be 1-128 visible ASCII characters.",
        ));
    }
    if !value
        .chars()
        .all(|character| ('!'..='~').contains(&character))
    {
        return Err(AppError::validation(
            "Idempotency-Key must be 1-128 visible ASCII characters.",
        ));
    }
    Ok(())
}

pub fn validate_message_input(input: &MessageInput) -> Result<(), AppError> {
    validate_sender(&input.sender)?;
    let subject_len = input.subject.chars().count();
    if subject_len == 0 || subject_len > MAX_SUBJECT_CHARS {
        return Err(AppError::validation(
            "subject must be 1-120 characters.",
        ));
    }
    let body_len = input.body.chars().count();
    if body_len == 0 || body_len > MAX_BODY_CHARS {
        return Err(AppError::validation("body must be 1-5000 characters."));
    }
    if input.attachments.len() > MAX_ATTACHMENTS {
        return Err(AppError::validation(
            "a message can contain at most 20 attachments.",
        ));
    }
    let mut seen = HashSet::new();
    for attachment in &input.attachments {
        if !seen.insert(attachment.identity()) {
            return Err(AppError::validation(
                "duplicate attachment type and id pairs are not allowed.",
            ));
        }
    }
    validate_metadata(&input.metadata)?;
    Ok(())
}

fn validate_sender(sender: &Sender) -> Result<(), AppError> {
    if sender.sender_type == SenderType::Player {
        return Err(AppError::validation(
            "Player-to-player mail is not available. Sender type must be system or admin.",
        ));
    }
    let name_len = sender.display_name.chars().count();
    if name_len == 0 || name_len > MAX_DISPLAY_NAME_CHARS {
        return Err(AppError::validation(
            "sender.displayName must be 1-120 characters.",
        ));
    }
    if let Some(id) = &sender.id {
        if !is_safe_id(id) {
            return Err(AppError::validation(
                "sender.id must be 1-128 characters of letters, digits, underscores, or hyphens, or null.",
            ));
        }
    }
    Ok(())
}

pub fn validate_metadata(metadata: &Map<String, Value>) -> Result<(), AppError> {
    for key in metadata.keys() {
        if RESERVED_METADATA_KEYS.contains(&key.as_str()) {
            return Err(AppError::validation(format!(
                "metadata must not contain the reserved field {key}."
            )));
        }
    }
    if !depth_within_limit(&Value::Object(metadata.clone()), 1, MAX_METADATA_DEPTH) {
        return Err(AppError::validation(
            "metadata nesting cannot be deeper than 8.",
        ));
    }
    let encoded = serde_json::to_vec(metadata).map_err(|_| AppError::internal())?;
    if encoded.len() > MAX_METADATA_BYTES {
        return Err(AppError::validation(
            "metadata must serialize to at most 2 KiB.",
        ));
    }
    Ok(())
}

fn depth_within_limit(value: &Value, depth: usize, max: usize) -> bool {
    if depth > max {
        return false;
    }
    match value {
        Value::Array(items) => items
            .iter()
            .all(|item| depth_within_limit(item, depth + 1, max)),
        Value::Object(map) => map
            .values()
            .all(|item| depth_within_limit(item, depth + 1, max)),
        _ => true,
    }
}

