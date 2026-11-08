use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::models::attachment::Attachment;
use crate::models::requests::MessageInput;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SenderType {
    System,
    Admin,
    Player,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Sender {
    #[serde(rename = "type")]
    pub sender_type: SenderType,
    pub id: Option<String>,
    pub display_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    General,
    Announcement,
    Reward,
    Compensation,
    Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageState {
    #[serde(rename = "unread")]
    Unread,
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "claim")]
    Claim,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Message {
    pub schema_version: u32,
    pub id: Uuid,
    pub sender: Sender,
    pub recipient_id: String,
    pub category: Category,
    pub subject: String,
    pub body: String,
    pub state: MessageState,
    #[serde(with = "crate::models::time::required")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "crate::models::time::optional")]
    pub read_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::models::time::optional")]
    pub claimed_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::models::time::optional")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Transition {
    Unchanged(Message),
    Updated(Message),
}

impl Message {
    pub fn create(id: Uuid, recipient_id: String, input: &MessageInput, now: DateTime<Utc>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id,
            sender: input.sender.clone(),
            recipient_id,
            category: input.category,
            subject: input.subject.clone(),
            body: input.body.clone(),
            state: MessageState::Unread,
            created_at: now,
            read_at: None,
            claimed_at: None,
            expires_at: input.expires_at,
            attachments: input.attachments.clone(),
            metadata: input.metadata.clone(),
        }
    }

    pub fn mark_read(&self, now: DateTime<Utc>) -> Transition {
        match self.state {
            MessageState::Unread => {
                let mut updated = self.clone();
                updated.state = MessageState::Read;
                updated.read_at = Some(now);
                Transition::Updated(updated)
            }
            MessageState::Read | MessageState::Claim => Transition::Unchanged(self.clone()),
        }
    }

    pub fn mark_claim(&self, now: DateTime<Utc>) -> Result<Transition, &'static str> {
        if self.attachments.is_empty() {
            return Err("A message without attachments cannot be claimed.");
        }
        match self.state {
            MessageState::Claim => Ok(Transition::Unchanged(self.clone())),
            MessageState::Unread | MessageState::Read => {
                let mut updated = self.clone();
                if updated.read_at.is_none() {
                    updated.read_at = Some(now);
                }
                updated.state = MessageState::Claim;
                updated.claimed_at = Some(now);
                Ok(Transition::Updated(updated))
            }
        }
    }

    pub fn has_unclaimed_attachments(&self) -> bool {
        !self.attachments.is_empty() && self.state != MessageState::Claim
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageList {
    pub player_id: String,
    pub total: usize,
    pub messages: Vec<Message>,
}
