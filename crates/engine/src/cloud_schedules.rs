//! The hosting machine's half of edge-owned Captain schedules (SUR-269,
//! complementing edge/src/cloud/schedules.ts). Two jobs:
//!
//! - **Forwarding**: on a Cloud machine, `schedule_create`/`edit`/`delete`
//!   for the chat it hosts are written to the edge (the system of record —
//!   the local runner fires only while this engine runs, and this engine is
//!   exactly the one that sleeps). Desktop engines are unchanged.
//! - **Consumption**: fired occurrences cross as durable wake-up rows. This
//!   machine pulls them over its runner bearer, materializes each as an
//!   ordinary queued message — the doc write only the host can make, since
//!   the edge's chat-room is an opaque relay by design — and acks. Unacked
//!   rows are retried edge-side with backoff, so the pull is idempotent in
//!   effect: a crash after the doc write but before the ack re-delivers,
//!   and the queue scan below keeps that a no-op rather than a duplicate.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use zeron_rpc::TokenSource;

use crate::EngineError;

/// How often the drain re-pulls as a backstop. The low-latency path is the
/// boot pull plus the edge's wake+nudge; this covers a nudge lost to a
/// socket gap while the machine stayed up.
const REPOLL_EVERY: Duration = Duration::from_secs(60);

/// The edge's `cloud_schedule` row, as the DO serializes it (snake_case).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudScheduleRow {
    pub id: String,
    pub chat_id: String,
    pub title: String,
    pub prompt: String,
    pub rrule: String,
    pub active: i64,
    pub next_fire_at: Option<i64>,
    pub last_fired_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The edge's `cloud_wakeup` row (pending ones are what the pull returns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudWakeupRow {
    pub fire_id: String,
    pub schedule_id: String,
    pub chat_id: String,
    pub occurrence_at: i64,
    pub prompt: String,
}

/// Runner-authenticated edge client for one hosted chat's schedules.
pub struct EdgeSchedules {
    edge_url: String,
    org_id: String,
    chat_id: String,
    tokens: Arc<dyn TokenSource>,
    http: reqwest::Client,
}

impl EdgeSchedules {
    pub fn new(
        edge_url: &str,
        org_id: &str,
        chat_id: &str,
        tokens: Arc<dyn TokenSource>,
    ) -> Self {
        Self {
            edge_url: edge_url.trim_end_matches('/').to_string(),
            org_id: org_id.to_string(),
            chat_id: chat_id.to_string(),
            tokens,
            http: crate::runner::http_client(),
        }
    }

    pub fn chat_id(&self) -> &str {
        &self.chat_id
    }

    fn schedules_url(&self) -> String {
        format!(
            "{}/cloud/{}/sessions/{}/schedules",
            self.edge_url, self.org_id, self.chat_id
        )
    }

    async fn token(&self) -> Result<String, EngineError> {
        self.tokens
            .token()
            .await
            .map_err(|err| EngineError::Other(format!("runner token for a schedule call: {err}")))
    }

    async fn read_error<T: for<'de> Deserialize<'de>>(
        res: reqwest::Response,
        what: &str,
    ) -> Result<T, EngineError> {
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .map_err(|err| EngineError::Other(format!("{what}: reply cut off: {err}")))?;
        if !status.is_success() {
            #[derive(Deserialize)]
            struct EdgeError {
                #[serde(default)]
                error: Option<String>,
                #[serde(default)]
                message: Option<String>,
            }
            let parsed = serde_json::from_slice::<EdgeError>(&bytes).ok();
            let detail = parsed
                .and_then(|body| body.message.or(body.error))
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            return Err(EngineError::Other(format!("{what}: {detail}")));
        }
        serde_json::from_slice(&bytes)
            .map_err(|err| EngineError::Other(format!("{what}: malformed reply: {err}")))
    }

    /// `POST /cloud/{org}/sessions/{chat}/schedules` — create or replace.
    pub async fn upsert(
        &self,
        id: Option<&str>,
        title: &str,
        prompt: &str,
        rrule: &str,
        active: bool,
    ) -> Result<CloudScheduleRow, EngineError> {
        let mut body = serde_json::json!({
            "title": title,
            "prompt": prompt,
            "rrule": rrule,
            "active": active,
        });
        if let Some(id) = id {
            body["id"] = serde_json::json!(id);
        }
        let res = self
            .http
            .post(self.schedules_url())
            .bearer_auth(self.token().await?)
            .json(&body)
            .send()
            .await
            .map_err(|err| EngineError::Other(format!("edge schedule save unreachable: {err}")))?;
        #[derive(Deserialize)]
        struct Saved {
            schedule: CloudScheduleRow,
        }
        Self::read_error::<Saved>(res, "edge schedule save")
            .await
            .map(|saved| saved.schedule)
    }

    /// `DELETE /cloud/{org}/sessions/{chat}/schedules/{id}`.
    pub async fn delete(&self, id: &str) -> Result<bool, EngineError> {
        let res = self
            .http
            .delete(format!("{}/{}", self.schedules_url(), id))
            .bearer_auth(self.token().await?)
            .send()
            .await
            .map_err(|err| EngineError::Other(format!("edge schedule delete unreachable: {err}")))?;
        #[derive(Deserialize)]
        struct Deleted {
            deleted: bool,
        }
        Self::read_error::<Deleted>(res, "edge schedule delete")
            .await
            .map(|reply| reply.deleted)
    }

    /// `GET /cloud/{org}/sessions/{chat}/schedules`.
    pub async fn list(&self) -> Result<Vec<CloudScheduleRow>, EngineError> {
        let res = self
            .http
            .get(self.schedules_url())
            .bearer_auth(self.token().await?)
            .send()
            .await
            .map_err(|err| EngineError::Other(format!("edge schedule list unreachable: {err}")))?;
        #[derive(Deserialize)]
        struct Listed {
            schedules: Vec<CloudScheduleRow>,
        }
        Self::read_error::<Listed>(res, "edge schedule list")
            .await
            .map(|reply| reply.schedules)
    }

    /// `POST /runner/wakeups {chatId}` — the pending wake-ups this machine
    /// owes its chat.
    pub async fn pull_wakeups(&self) -> Result<Vec<CloudWakeupRow>, EngineError> {
        let res = self
            .http
            .post(format!("{}/runner/wakeups", self.edge_url))
            .bearer_auth(self.token().await?)
            .json(&serde_json::json!({ "chatId": self.chat_id }))
            .send()
            .await
            .map_err(|err| EngineError::Other(format!("wake-up pull unreachable: {err}")))?;
        #[derive(Deserialize)]
        struct Pulled {
            wakeups: Vec<CloudWakeupRow>,
        }
        Self::read_error::<Pulled>(res, "wake-up pull")
            .await
            .map(|reply| reply.wakeups)
    }

    /// `POST /runner/wakeups/ack {chatId, fireId}` — after the prompt landed
    /// in the chat's queue, never before.
    pub async fn ack_wakeup(&self, fire_id: &str) -> Result<(), EngineError> {
        let res = self
            .http
            .post(format!("{}/runner/wakeups/ack", self.edge_url))
            .bearer_auth(self.token().await?)
            .json(&serde_json::json!({ "chatId": self.chat_id, "fireId": fire_id }))
            .send()
            .await
            .map_err(|err| EngineError::Other(format!("wake-up ack unreachable: {err}")))?;
        #[derive(Deserialize)]
        struct Acked {
            #[allow(dead_code)]
            acked: bool,
        }
        Self::read_error::<Acked>(res, "wake-up ack").await?;
        Ok(())
    }
}

/// One drain pass: every pending wake-up becomes an ordinary queued message
/// on the hosted chat, then an ack. Returns how many landed.
pub async fn drain_wakeups(
    doc_host: &crate::doc_host::DocHost,
    edge: &EdgeSchedules,
) -> Result<usize, EngineError> {
    let wakeups = edge.pull_wakeups().await?;
    let mut landed = 0;
    for wakeup in wakeups {
        // Idempotency: a crash after the doc write but before the ack is the
        // one replay case, and the edge's retries re-send the same row. An
        // identical prompt already waiting in the queue makes this one a
        // no-op rather than a duplicate.
        let already = doc_host
            .open(&wakeup.chat_id)
            .and_then(|handle| {
                handle
                    .doc()
                    .read_queue()
                    .map_err(|err| EngineError::Other(err.to_string()))
            })
            .map(|queue| queue.iter().any(|row| row.text == wakeup.prompt))
            .unwrap_or(false);
        if !already {
            doc_host.queue_message(&wakeup.chat_id, &wakeup.prompt, Vec::new())?;
        }
        edge.ack_wakeup(&wakeup.fire_id).await?;
        landed += 1;
    }
    Ok(landed)
}

/// The backstop loop: drain now — the boot case, a machine woken FOR a
/// wake-up — then once a minute. Errors are debug-quiet: a machine offline
/// from the edge retries at the next pass, and the edge's own retry sweep
/// re-wakes regardless.
pub fn spawn_wakeup_drain(
    doc_host: crate::doc_host::DocHost,
    edge: Arc<EdgeSchedules>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match drain_wakeups(&doc_host, &edge).await {
                Ok(landed) if landed > 0 => {
                    tracing::info!(landed, "cloud schedules: wake-ups materialized")
                }
                Err(err) => {
                    tracing::debug!(error = %err, "cloud schedules: wake-up drain failed")
                }
                _ => {}
            }
            tokio::time::sleep(REPOLL_EVERY).await;
        }
    })
}
