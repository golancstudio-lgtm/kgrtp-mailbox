use std::sync::Arc;

use crate::admin::{GoogleVerifier, SessionIssuer};
use crate::config::Config;
use crate::services::{BroadcastService, MailboxService};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub mailbox: MailboxService,
    pub broadcast: BroadcastService,
    pub sessions: SessionIssuer,
    pub google: GoogleVerifier,
}
