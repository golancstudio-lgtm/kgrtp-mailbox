use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock, watch};
use uuid::Uuid;

use crate::models::time;
use crate::models::{
    AdminBroadcastStatus, AppError, BroadcastFailure, BroadcastStatus, BroadcastStatusKind,
    Message, MessageInput, RecipientFailure,
};
use crate::services::mailbox::{MailboxService, PreparedDelivery};
use crate::services::player_validation::PlayerValidation;
use crate::storage::{MailboxStore, JOBS_CUSTOM_ID};
use crate::ugs::{map_transport, CallMode, PlayerAuth, UgsError};
use crate::util::sha256_hex;
use crate::validation::{is_safe_id, validate_message_input};

const BROADCAST_NAMESPACE: Uuid = Uuid::from_u128(0x6f1b9c3e8a4d4e2b9c1f7a2d5e8b0c44);
const ACTIVE_INDEX_KEY: &str = "active-index";
const WORKER_BATCH: usize = 8;
const PLAYER_INTERVAL: Duration = Duration::from_millis(100);
const PAGE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_STORED_FAILURES: usize = 500;
const MAX_PLAYER_PAGES: u32 = 100_000;

#[derive(Debug)]
pub struct BroadcastAcceptance {
    pub broadcast_id: Uuid,
    pub created: bool,
}

#[derive(Clone)]
pub struct BroadcastService {
    inner: Arc<Inner>,
}

struct Inner {
    cloud: MailboxStore,
    players: PlayerAuth,
    mailbox: MailboxService,
    validation: PlayerValidation,
    instance_id: String,
    notify: Notify,
    shutdown: AtomicBool,
    done: watch::Sender<bool>,
    last_pace: RwLock<Instant>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BroadcastJob {
    job_version: u32,
    broadcast_id: Uuid,
    status: BroadcastStatusKind,
    template: MessageInput,
    body_sha256: String,
    #[serde(default)]
    page_token: Option<String>,
    #[serde(default)]
    list_complete: bool,
    #[serde(default)]
    pending_player_ids: Vec<String>,
    #[serde(default)]
    pages_read: u32,
    #[serde(default)]
    total_players: Option<u64>,
    #[serde(default)]
    succeeded: u64,
    #[serde(default)]
    failed: u64,
    #[serde(default)]
    failures: Vec<RecipientFailure>,
    #[serde(default)]
    failures_truncated: bool,
    #[serde(default)]
    consecutive_outages: u32,
    #[serde(default)]
    lease_owner: Option<String>,
    #[serde(default, with = "time::optional")]
    lease_until: Option<DateTime<Utc>>,
    #[serde(default, with = "time::optional")]
    not_before: Option<DateTime<Utc>>,
    #[serde(default)]
    failure_code: Option<String>,
    #[serde(default)]
    failure_message: Option<String>,
    #[serde(with = "time::required")]
    created_at: DateTime<Utc>,
    #[serde(with = "time::required")]
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ActiveIndex {
    #[serde(default)]
    ids: Vec<Uuid>,
}

enum BatchStop {
    Continue,
    Stop,
}

impl BroadcastService {
    pub fn new(
        cloud: MailboxStore,
        players: PlayerAuth,
        mailbox: MailboxService,
        validation: PlayerValidation,
    ) -> Self {
        let (done, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                cloud,
                players,
                mailbox,
                validation,
                instance_id: Uuid::new_v4().to_string(),
                notify: Notify::new(),
                shutdown: AtomicBool::new(false),
                done,
                last_pace: RwLock::new(Instant::now() - PLAYER_INTERVAL),
            }),
        }
    }

    pub fn spawn(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            service.run().await;
            let _ = service.inner.done.send(true);
        });
    }

    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub async fn stopped(&self) {
        let mut receiver = self.inner.done.subscribe();
        if *receiver.borrow() {
            return;
        }
        let _ = receiver.changed().await;
    }

    pub fn nudge(&self) {
        self.inner.notify.notify_one();
    }

    pub async fn accept(
        &self,
        input: &MessageInput,
        raw_body: &[u8],
        idempotency_key: Option<&str>,
    ) -> Result<BroadcastAcceptance, AppError> {
        validate_message_input(input)?;
        let body_sha256 = sha256_hex(raw_body);
        let broadcast_id = match idempotency_key {
            Some(key) => {
                crate::validation::validate_idempotency_key(key)?;
                Uuid::new_v5(&BROADCAST_NAMESPACE, format!("broadcast:{key}").as_bytes())
            }
            None => Uuid::new_v4(),
        };
        if let Some((existing, _)) = self.load_job(broadcast_id).await? {
            if existing.body_sha256 != body_sha256 {
                return Err(AppError::idempotency_conflict(
                    "This Idempotency-Key was already used with a different broadcast body.",
                ));
            }
            return Ok(BroadcastAcceptance {
                broadcast_id,
                created: false,
            });
        }
        let now = Utc::now();
        let job = BroadcastJob {
            job_version: 1,
            broadcast_id,
            status: BroadcastStatusKind::Pending,
            template: input.clone(),
            body_sha256,
            page_token: None,
            list_complete: false,
            pending_player_ids: Vec::new(),
            pages_read: 0,
            total_players: None,
            succeeded: 0,
            failed: 0,
            failures: Vec::new(),
            failures_truncated: false,
            consecutive_outages: 0,
            lease_owner: None,
            lease_until: None,
            not_before: None,
            failure_code: None,
            failure_message: None,
            created_at: now,
            updated_at: now,
        };
        self.save_job(&job, None).await?;
        let Some((confirmed, _)) = self.load_job(broadcast_id).await? else {
            return Err(AppError::bad_gateway());
        };
        if confirmed.body_sha256 != job.body_sha256 {
            return Err(AppError::idempotency_conflict(
                "This Idempotency-Key was claimed by a different broadcast body.",
            ));
        }
        if let Err(error) = self.add_active(broadcast_id).await {
            tracing::warn!(%broadcast_id, error = %error, "broadcast index update failed; reconciliation will retry");
        }
        self.nudge();
        Ok(BroadcastAcceptance {
            broadcast_id,
            created: true,
        })
    }

    pub async fn status(&self, broadcast_id: Uuid) -> Result<BroadcastStatus, AppError> {
        self.nudge();
        if self.lease_is_free(broadcast_id).await? {
            let _ = self.run_batches(broadcast_id, 1, 1).await;
        }
        let (job, _) = self
            .load_job(broadcast_id)
            .await?
            .ok_or_else(AppError::broadcast_not_found)?;
        Ok(job.public_status())
    }

    pub async fn admin_status(&self, broadcast_id: Uuid) -> Result<AdminBroadcastStatus, AppError> {
        let status = self.status(broadcast_id).await?;
        let (job, _) = self
            .load_job(broadcast_id)
            .await?
            .ok_or_else(AppError::broadcast_not_found)?;
        let recent = job.failures.iter().rev().take(10).cloned().collect();
        Ok(AdminBroadcastStatus {
            status,
            failures_truncated: job.failures_truncated,
            recent_failures: recent,
        })
    }

    pub async fn validate_player(&self, player_id: &str) -> Result<serde_json::Value, AppError> {
        let player = self.inner.validation.require_existing(player_id).await?;
        Ok(serde_json::json!({
            "playerId": player.id,
            "exists": true,
            "disabled": player.disabled
        }))
    }

    async fn run(&self) {
        let mut last_reconcile = Instant::now() - Duration::from_secs(600);
        loop {
            if self.inner.shutdown.load(Ordering::SeqCst) {
                break;
            }
            if last_reconcile.elapsed() >= Duration::from_secs(600) {
                if let Err(error) = self.reconcile().await {
                    tracing::warn!(error = %error, "broadcast reconciliation failed");
                }
                last_reconcile = Instant::now();
            }
            match self.next_runnable().await {
                Ok(Some(id)) => {
                    if let Err(error) = self.run_batches(id, WORKER_BATCH, usize::MAX).await {
                        tracing::warn!(%id, error = %error, "broadcast batch failed");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
                Ok(None) => self.wait_for_work().await,
                Err(error) => {
                    tracing::warn!(error = %error, "broadcast worker could not list jobs");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn wait_for_work(&self) {
        tokio::select! {
            _ = self.inner.notify.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }

    async fn run_batches(
        &self,
        broadcast_id: Uuid,
        batch_size: usize,
        max_batches: usize,
    ) -> Result<(), AppError> {
        for _ in 0..max_batches {
            if self.inner.shutdown.load(Ordering::SeqCst) {
                return Ok(());
            }
            match self.one_batch(broadcast_id, batch_size).await? {
                BatchStop::Continue => {}
                BatchStop::Stop => return Ok(()),
            }
        }
        Ok(())
    }

    async fn one_batch(&self, broadcast_id: Uuid, batch_size: usize) -> Result<BatchStop, AppError> {
        let Some((mut job, mut lock)) = self.load_job(broadcast_id).await? else {
            return Ok(BatchStop::Stop);
        };
        if job.status.is_terminal() {
            return Ok(BatchStop::Stop);
        }
        if job.not_before.is_some_and(|instant| instant > Utc::now()) {
            return Ok(BatchStop::Stop);
        }
        if !self.owns_or_expired(&job) {
            return Ok(BatchStop::Stop);
        }
        self.hold_lease(&mut job);
        let previous_lock = lock.clone();
        lock = match self.save_job(&job, previous_lock.as_deref()).await? {
            Some(next) => Some(next),
            None => self.reload_lock(broadcast_id).await?,
        };

        if job.pending_player_ids.is_empty() {
            if job.list_complete {
                self.finish_job(&mut job, lock.as_deref()).await?;
                return Ok(BatchStop::Stop);
            }
            match self.fill_players(&mut job).await {
                Ok(()) => {
                    self.save_job(&job, lock.as_deref()).await?;
                    return Ok(BatchStop::Continue);
                }
                Err(error) => {
                    return self.pause_or_fail(&mut job, lock.as_deref(), error).await;
                }
            }
        }

        let take = batch_size.min(job.pending_player_ids.len());
        let batch: Vec<String> = job.pending_player_ids.iter().take(take).cloned().collect();
        let mut completed = 0usize;
        for player_id in &batch {
            self.pace().await;
            let message = message_for(&job, player_id);
            match self
                .inner
                .mailbox
                .deliver_prepared(&message, CallMode::Background)
                .await
            {
                Ok(PreparedDelivery::Created | PreparedDelivery::AlreadyPresent) => {
                    job.succeeded += 1;
                    job.consecutive_outages = 0;
                    job.not_before = None;
                    completed += 1;
                }
                Err(error) => match disposition(&error) {
                    Disposition::Permanent => {
                        push_failure(&mut job, player_id, &error);
                        job.consecutive_outages = 0;
                        completed += 1;
                    }
                    Disposition::Retry {
                        after_secs,
                        counts_toward_outage,
                    } => {
                        if counts_toward_outage {
                            job.consecutive_outages += 1;
                        }
                        job.pending_player_ids = batch[completed..]
                            .iter()
                            .cloned()
                            .chain(job.pending_player_ids.iter().skip(take).cloned())
                            .collect();
                        if job.consecutive_outages >= 5 {
                            fail_job(
                                &mut job,
                                error.code,
                                "Unity Gaming Services remained unavailable. Confirmed deliveries were kept.",
                            );
                            self.save_job(&job, lock.as_deref()).await?;
                            self.remove_active(broadcast_id).await.ok();
                            return Ok(BatchStop::Stop);
                        }
                        job.not_before = Some(Utc::now() + chrono::Duration::seconds(after_secs as i64));
                        self.hold_lease(&mut job);
                        self.save_job(&job, lock.as_deref()).await?;
                        return Ok(BatchStop::Stop);
                    }
                },
            }
        }
        let remaining = job.pending_player_ids.split_off(take);
        job.pending_player_ids = remaining;
        let _ = completed;
        job.not_before = None;
        self.hold_lease(&mut job);
        if job.pending_player_ids.is_empty() && job.list_complete {
            self.finish_job(&mut job, lock.as_deref()).await?;
            return Ok(BatchStop::Stop);
        }
        self.save_job(&job, lock.as_deref()).await?;
        Ok(BatchStop::Continue)
    }

    async fn fill_players(&self, job: &mut BroadcastJob) -> Result<(), AppError> {
        if job.pages_read >= MAX_PLAYER_PAGES {
            return Err(AppError::ugs_unavailable(
                "Player listing exceeded the safety page limit before the broadcast could finish.",
            ));
        }
        if job.pages_read > 0 {
            tokio::time::sleep(PAGE_INTERVAL).await;
        }
        let page = match self
            .inner
            .players
            .list_players(job.page_token.as_deref(), CallMode::Background)
            .await
        {
            Ok(page) => page,
            Err(UgsError::NotFound) => {
                job.list_complete = true;
                job.page_token = None;
                job.total_players = Some(job.succeeded + job.failed + job.pending_player_ids.len() as u64);
                return Ok(());
            }
            Err(error) => return Err(map_transport(error)),
        };
        job.pages_read += 1;
        let mut seen = HashSet::new();
        for player in page.players {
            if player.disabled || !is_safe_id(&player.id) || !seen.insert(player.id.clone()) {
                continue;
            }
            job.pending_player_ids.push(player.id);
        }
        match page.next {
            Some(next) if job.page_token.as_ref() == Some(&next) => {
                job.list_complete = true;
                job.page_token = None;
            }
            Some(next) => job.page_token = Some(next),
            None => {
                job.list_complete = true;
                job.page_token = None;
            }
        }
        if job.list_complete {
            job.total_players = Some(job.succeeded + job.failed + job.pending_player_ids.len() as u64);
        }
        job.updated_at = Utc::now();
        Ok(())
    }

    async fn pause_or_fail(
        &self,
        job: &mut BroadcastJob,
        lock: Option<&str>,
        error: AppError,
    ) -> Result<BatchStop, AppError> {
        match disposition(&error) {
            Disposition::Permanent => {
                fail_job(job, error.code, &error.message);
                self.save_job(job, lock).await?;
                self.remove_active(job.broadcast_id).await.ok();
                Ok(BatchStop::Stop)
            }
            Disposition::Retry {
                after_secs,
                counts_toward_outage,
            } => {
                if counts_toward_outage {
                    job.consecutive_outages += 1;
                }
                if job.consecutive_outages >= 5 {
                    fail_job(
                        job,
                        error.code,
                        "Unity Gaming Services remained unavailable while listing players.",
                    );
                    self.save_job(job, lock).await?;
                    self.remove_active(job.broadcast_id).await.ok();
                    return Ok(BatchStop::Stop);
                }
                job.not_before = Some(Utc::now() + chrono::Duration::seconds(after_secs as i64));
                job.updated_at = Utc::now();
                self.save_job(job, lock).await?;
                Ok(BatchStop::Stop)
            }
        }
    }

    async fn finish_job(&self, job: &mut BroadcastJob, lock: Option<&str>) -> Result<(), AppError> {
        job.total_players = Some(job.succeeded + job.failed);
        job.status = if job.failed > 0 {
            BroadcastStatusKind::CompletedWithErrors
        } else {
            BroadcastStatusKind::Completed
        };
        job.lease_owner = None;
        job.lease_until = None;
        job.not_before = None;
        job.failure_code = None;
        job.failure_message = None;
        job.updated_at = Utc::now();
        self.save_job(job, lock).await?;
        self.remove_active(job.broadcast_id).await.ok();
        Ok(())
    }

    fn hold_lease(&self, job: &mut BroadcastJob) {
        job.lease_owner = Some(self.inner.instance_id.clone());
        job.lease_until = Some(Utc::now() + chrono::Duration::seconds(180));
        if !job.status.is_terminal() {
            job.status = BroadcastStatusKind::Processing;
        }
        job.updated_at = Utc::now();
    }

    fn owns_or_expired(&self, job: &BroadcastJob) -> bool {
        match (&job.lease_owner, job.lease_until) {
            (Some(owner), Some(until)) if until > Utc::now() && owner != &self.inner.instance_id => false,
            _ => true,
        }
    }

    async fn lease_is_free(&self, broadcast_id: Uuid) -> Result<bool, AppError> {
        let Some((job, _)) = self.load_job(broadcast_id).await? else {
            return Ok(false);
        };
        if job.status.is_terminal() || job.not_before.is_some_and(|instant| instant > Utc::now()) {
            return Ok(false);
        }
        Ok(self.owns_or_expired(&job))
    }

    async fn next_runnable(&self) -> Result<Option<Uuid>, AppError> {
        let index = self.load_index().await?;
        for id in index.ids {
            let Some((job, _)) = self.load_job(id).await? else {
                continue;
            };
            if job.status.is_terminal() {
                continue;
            }
            if job.not_before.is_some_and(|instant| instant > Utc::now()) {
                continue;
            }
            if self.owns_or_expired(&job) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    async fn reconcile(&self) -> Result<(), AppError> {
        let items = self
            .inner
            .cloud
            .list_private(JOBS_CUSTOM_ID, CallMode::Background)
            .await
            .map_err(map_job_storage)?;
        let mut active = Vec::new();
        let mut terminal = Vec::new();
        for item in items {
            if !item.key.starts_with("bcast_") {
                continue;
            }
            let Some(value) = item.value else {
                continue;
            };
            let Ok(job) = serde_json::from_value::<BroadcastJob>(value) else {
                continue;
            };
            if job.status.is_terminal() {
                terminal.push(job.broadcast_id);
            } else {
                active.push(job.broadcast_id);
            }
        }
        for _ in 0..4 {
            let (mut index, lock) = self.load_index_locked().await?;
            index.ids.retain(|id| !terminal.contains(id));
            for id in &active {
                if !index.ids.contains(id) {
                    index.ids.push(*id);
                }
            }
            index.ids.sort();
            index.ids.dedup();
            let value = serde_json::to_value(&index).map_err(|_| AppError::internal())?;
            match self
                .inner
                .cloud
                .set_private(
                    JOBS_CUSTOM_ID,
                    ACTIVE_INDEX_KEY,
                    &value,
                    lock.as_deref(),
                    CallMode::Background,
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(UgsError::Conflict) => continue,
                Err(error) => return Err(map_job_storage(error)),
            }
        }
        Err(AppError::write_conflict())
    }

    async fn add_active(&self, broadcast_id: Uuid) -> Result<(), AppError> {
        for _ in 0..4 {
            let (mut index, lock) = self.load_index_locked().await?;
            if !index.ids.contains(&broadcast_id) {
                index.ids.push(broadcast_id);
            }
            let value = serde_json::to_value(&index).map_err(|_| AppError::internal())?;
            match self
                .inner
                .cloud
                .set_private(
                    JOBS_CUSTOM_ID,
                    ACTIVE_INDEX_KEY,
                    &value,
                    lock.as_deref(),
                    CallMode::Interactive,
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(UgsError::Conflict) => continue,
                Err(error) => return Err(map_job_storage(error)),
            }
        }
        Err(AppError::write_conflict())
    }

    async fn remove_active(&self, broadcast_id: Uuid) -> Result<(), AppError> {
        for _ in 0..4 {
            let (mut index, lock) = self.load_index_locked().await?;
            index.ids.retain(|id| *id != broadcast_id);
            let value = serde_json::to_value(&index).map_err(|_| AppError::internal())?;
            match self
                .inner
                .cloud
                .set_private(
                    JOBS_CUSTOM_ID,
                    ACTIVE_INDEX_KEY,
                    &value,
                    lock.as_deref(),
                    CallMode::Background,
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(UgsError::Conflict) => continue,
                Err(error) => return Err(map_job_storage(error)),
            }
        }
        Err(AppError::write_conflict())
    }

    async fn load_index(&self) -> Result<ActiveIndex, AppError> {
        Ok(self.load_index_locked().await?.0)
    }

    async fn load_index_locked(&self) -> Result<(ActiveIndex, Option<String>), AppError> {
        let item = self
            .inner
            .cloud
            .get_private(JOBS_CUSTOM_ID, ACTIVE_INDEX_KEY, CallMode::Background)
            .await
            .map_err(map_job_storage)?;
        let Some(item) = item else {
            return Ok((ActiveIndex::default(), None));
        };
        let index = match item.value {
            Some(value) => serde_json::from_value(value).unwrap_or_default(),
            None => ActiveIndex::default(),
        };
        Ok((index, item.write_lock))
    }

    async fn load_job(&self, broadcast_id: Uuid) -> Result<Option<(BroadcastJob, Option<String>)>, AppError> {
        let item = self
            .inner
            .cloud
            .get_private(JOBS_CUSTOM_ID, &job_key(broadcast_id), CallMode::Background)
            .await
            .map_err(map_job_storage)?;
        let Some(item) = item else {
            return Ok(None);
        };
        let Some(value) = item.value else {
            return Err(AppError::bad_gateway());
        };
        let job = serde_json::from_value(value).map_err(|_| AppError::bad_gateway())?;
        Ok(Some((job, item.write_lock)))
    }

    async fn reload_lock(&self, broadcast_id: Uuid) -> Result<Option<String>, AppError> {
        Ok(self.load_job(broadcast_id).await?.and_then(|(_, lock)| lock))
    }

    async fn save_job(
        &self,
        job: &BroadcastJob,
        write_lock: Option<&str>,
    ) -> Result<Option<String>, AppError> {
        let value = serde_json::to_value(job).map_err(|_| AppError::internal())?;
        self.inner
            .cloud
            .set_private(
                JOBS_CUSTOM_ID,
                &job_key(job.broadcast_id),
                &value,
                write_lock,
                CallMode::Background,
            )
            .await
            .map_err(map_job_storage)
    }

    async fn pace(&self) {
        let mut last = self.inner.last_pace.write().await;
        let elapsed = last.elapsed();
        if elapsed < PLAYER_INTERVAL {
            tokio::time::sleep(PLAYER_INTERVAL - elapsed).await;
        }
        *last = Instant::now();
    }
}

impl BroadcastJob {
    fn public_status(&self) -> BroadcastStatus {
        BroadcastStatus {
            broadcast_id: self.broadcast_id,
            status: self.status,
            total_players: self.total_players,
            processed: self.succeeded + self.failed,
            succeeded: self.succeeded,
            failed: self.failed,
            failure: match (&self.failure_code, &self.failure_message) {
                (Some(code), Some(message)) if self.status == BroadcastStatusKind::Failed => {
                    Some(BroadcastFailure {
                        code: code.clone(),
                        message: message.clone(),
                    })
                }
                _ => None,
            },
        }
    }
}

enum Disposition {
    Permanent,
    Retry { after_secs: u64, counts_toward_outage: bool },
}

fn disposition(error: &AppError) -> Disposition {
    match error.code {
        "RATE_LIMITED" => Disposition::Retry {
            after_secs: error.retry_after.unwrap_or(30).clamp(1, 900),
            counts_toward_outage: false,
        },
        "UGS_TIMEOUT" | "UGS_UNAVAILABLE" => Disposition::Retry {
            after_secs: 20,
            counts_toward_outage: true,
        },
        _ => Disposition::Permanent,
    }
}

fn push_failure(job: &mut BroadcastJob, player_id: &str, error: &AppError) {
    job.failed += 1;
    if job.failures.len() < MAX_STORED_FAILURES {
        job.failures.push(RecipientFailure {
            player_id: player_id.to_owned(),
            code: error.code.to_owned(),
            message: error.message.clone(),
        });
    } else {
        job.failures_truncated = true;
    }
}

fn fail_job(job: &mut BroadcastJob, code: &str, message: &str) {
    job.status = BroadcastStatusKind::Failed;
    job.failure_code = Some(code.to_owned());
    job.failure_message = Some(message.to_owned());
    job.lease_owner = None;
    job.lease_until = None;
    job.updated_at = Utc::now();
}

fn message_for(job: &BroadcastJob, player_id: &str) -> Message {
    let message_id = Uuid::new_v5(
        &BROADCAST_NAMESPACE,
        format!("v1:{}:{player_id}", job.broadcast_id).as_bytes(),
    );
    Message::create(
        message_id,
        player_id.to_owned(),
        &job.template,
        Utc::now(),
    )
}

fn job_key(broadcast_id: Uuid) -> String {
    format!("bcast_{broadcast_id}")
}

fn map_job_storage(error: UgsError) -> AppError {
    match error {
        UgsError::StorageLimit => AppError::conflict(
            "STORAGE_LIMIT_REACHED",
            "Broadcast storage rejected the job record. No broadcast records were deleted.",
        ),
        UgsError::NotFound => AppError::broadcast_not_found(),
        other => map_transport(other),
    }
}
