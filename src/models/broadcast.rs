use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastStatusKind {
    Pending,
    Processing,
    Completed,
    CompletedWithErrors,
    Failed,
}

impl BroadcastStatusKind {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::CompletedWithErrors | Self::Failed
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BroadcastFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BroadcastStatus {
    pub broadcast_id: Uuid,
    pub status: BroadcastStatusKind,
    pub total_players: Option<u64>,
    pub processed: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub failure: Option<BroadcastFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientFailure {
    pub player_id: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminBroadcastStatus {
    #[serde(flatten)]
    pub status: BroadcastStatus,
    pub failures_truncated: bool,
    pub recent_failures: Vec<RecipientFailure>,
}
