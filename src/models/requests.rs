use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::models::attachment::Attachment;
use crate::models::message::{Category, Sender};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MessageInput {
    pub sender: Sender,
    pub category: Category,
    pub subject: String,
    pub body: String,
    #[serde(default, with = "crate::models::time::optional")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}
