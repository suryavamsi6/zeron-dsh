//! Settings → Automation tasks' user-facing schedule surface (SUR-271).
//!
//! `ScheduleMcp` is the Captain-facing surface; this module is the desktop
//! window's surface. Local rows stay in this device's `schedules.json`. Rows
//! whose target chat is Cloud-hosted belong to the edge and are read and
//! written through the signed-in user's Cloud bearer — never through the
//! machine-scoped runner bearer, which a desktop does not possess.
//!
//! Ownership is fixed where the task was created: an existing row keeps its
//! store even if its chat later moves between desktop and Cloud. A manual
//! run is still local-only because the edge has no run-now mutation.

use serde_json::{Value, json};
use zeron_rpc::{RpcError, methods};

use crate::cloud_client::CloudClient;
use crate::cloud_schedules::CloudScheduleRow;
use crate::schedules::{Schedules, Target, Task};

/// Answer one Settings schedule RPC, merging or forwarding edge-owned rows
/// where needed. `cloud` is the user-auth client; when unavailable it simply
/// contributes no sessions or rows.
pub async fn call(
    schedules: &Schedules,
    cloud: CloudClient,
    method: &str,
    params: &Value,
) -> Result<Value, RpcError> {
    match method {
        methods::SCHEDULES => list(schedules, cloud).await,
        methods::SCHEDULE_SAVE => save(schedules, cloud, params).await,
        methods::SCHEDULE_DELETE => delete(schedules, cloud, params).await,
        methods::SCHEDULE_RUN_NOW => run_now(schedules, cloud, params).await,
        other => Err(RpcError::UnknownMethod(other.into())),
    }
}

fn failed(error: crate::EngineError) -> RpcError {
    RpcError::Failed(error.to_string())
}

fn bad(message: impl Into<String>) -> RpcError {
    RpcError::BadParams(message.into())
}

fn local_tasks(schedules: &Schedules) -> Result<Vec<Task>, RpcError> {
    let listed = crate::schedules::call(schedules, methods::SCHEDULES, &json!({}))?;
    serde_json::from_value(listed["tasks"].clone())
        .map_err(|error| RpcError::Failed(format!("local schedules malformed: {error}")))
}

async fn sessions(cloud: &CloudClient) -> Result<Vec<zeron_proto::CloudSession>, RpcError> {
    let sessions = cloud.sessions().await.map_err(failed)?;
    if !sessions.available {
        return Ok(Vec::new());
    }
    Ok(sessions.sessions)
}

async fn edge_rows_for(
    cloud: &CloudClient,
    sessions: &[zeron_proto::CloudSession],
) -> Result<Vec<CloudScheduleRow>, RpcError> {
    let mut rows = Vec::new();
    for session in sessions {
        rows.extend(
            cloud
                .list_schedules(&session.chat_id)
                .await
                .map_err(failed)?,
        );
    }
    Ok(rows)
}

async fn edge_rows(
    cloud: &CloudClient,
) -> Result<(Vec<zeron_proto::CloudSession>, Vec<CloudScheduleRow>), RpcError> {
    let sessions = sessions(cloud).await?;
    let rows = edge_rows_for(cloud, &sessions).await?;
    Ok((sessions, rows))
}

fn edge_task(row: &CloudScheduleRow) -> Task {
    crate::schedule_mcp::cloud_task(row)
}

fn parse_id<'a>(params: &'a Value, method: &str) -> Result<&'a str, RpcError> {
    params
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("{method} takes an id")))
}

fn parse_task(params: &Value) -> Result<Task, RpcError> {
    serde_json::from_value(params.get("task").cloned().unwrap_or(Value::Null))
        .map_err(|error| bad(format!("bad task: {error}")))
}

fn target_session(task: &Task) -> Option<&str> {
    match &task.target {
        Target::Session { session } => Some(session),
        Target::NewOnce { .. } | Target::New { .. } => None,
    }
}

async fn list(schedules: &Schedules, cloud: CloudClient) -> Result<Value, RpcError> {
    let mut tasks = local_tasks(schedules)?;
    let (_, rows) = edge_rows(&cloud).await?;
    tasks.extend(rows.iter().map(edge_task));
    serde_json::to_value(json!({ "tasks": tasks }))
        .map_err(|error| RpcError::Failed(error.to_string()))
}

async fn save(
    schedules: &Schedules,
    cloud: CloudClient,
    params: &Value,
) -> Result<Value, RpcError> {
    let task = parse_task(params)?;
    let local = local_tasks(schedules)?;

    // An id already in the device store is device-owned even when its target
    // chat is Cloud-hosted now: rows do not migrate with their chat.
    if !task.id.is_empty() && local.iter().any(|stored| stored.id == task.id) {
        return crate::schedules::call(schedules, methods::SCHEDULE_SAVE, params);
    }

    let (sessions, rows) = edge_rows(&cloud).await?;
    let edge = (!task.id.is_empty())
        .then(|| rows.iter().find(|row| row.id == task.id))
        .flatten();

    if let Some(existing) = edge {
        if target_session(&task) != Some(existing.chat_id.as_str()) {
            return Err(bad(
                "edge-owned schedules keep the Cloud chat they were created for",
            ));
        }
        let saved = cloud
            .upsert_schedule(
                &existing.chat_id,
                Some(&existing.id),
                &task.title,
                &task.prompt,
                &task.rrule,
                task.active,
            )
            .await
            .map_err(failed)?;
        return serde_json::to_value(edge_task(&saved))
            .map_err(|error| RpcError::Failed(error.to_string()));
    }

    // A genuinely new direct-chat schedule belongs at the edge exactly when
    // its target chat is Cloud-hosted. `new` and `new_once` create local
    // chats, so they remain device-owned.
    if let Some(chat_id) = target_session(&task) {
        if sessions.iter().any(|session| session.chat_id == chat_id) {
            let saved = cloud
                .upsert_schedule(
                    chat_id,
                    None,
                    &task.title,
                    &task.prompt,
                    &task.rrule,
                    task.active,
                )
                .await
                .map_err(failed)?;
            return serde_json::to_value(edge_task(&saved))
                .map_err(|error| RpcError::Failed(error.to_string()));
        }
    }

    crate::schedules::call(schedules, methods::SCHEDULE_SAVE, params)
}

async fn delete(
    schedules: &Schedules,
    cloud: CloudClient,
    params: &Value,
) -> Result<Value, RpcError> {
    let id = parse_id(params, "delete")?;
    if local_tasks(schedules)?.iter().any(|task| task.id == id) {
        return crate::schedules::call(schedules, methods::SCHEDULE_DELETE, params);
    }
    let (_, rows) = edge_rows(&cloud).await?;
    if let Some(row) = rows.iter().find(|row| row.id == id) {
        let deleted = cloud
            .delete_schedule(&row.chat_id, &row.id)
            .await
            .map_err(failed)?;
        return Ok(json!({ "deleted": deleted }));
    }
    crate::schedules::call(schedules, methods::SCHEDULE_DELETE, params)
}

async fn run_now(
    schedules: &Schedules,
    cloud: CloudClient,
    params: &Value,
) -> Result<Value, RpcError> {
    let id = parse_id(params, "runNow")?;
    if local_tasks(schedules)?.iter().any(|task| task.id == id) {
        return crate::schedules::call(schedules, methods::SCHEDULE_RUN_NOW, params);
    }
    let (_, rows) = edge_rows(&cloud).await?;
    if rows.iter().any(|row| row.id == id) {
        return Err(RpcError::Failed(
            "Run now isn't available for edge-owned schedules yet".into(),
        ));
    }
    crate::schedules::call(schedules, methods::SCHEDULE_RUN_NOW, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::schedules::{Deliver, Delivery};

    struct NoDeliver;
    impl Deliver for NoDeliver {
        fn deliver(&self, _task: &Task) -> Result<Delivery, crate::EngineError> {
            Ok(Delivery::Delivered {
                session: "local-chat".into(),
                row: "row".into(),
            })
        }
        fn pending(&self, _session: &str, _row: &str) -> Result<bool, crate::EngineError> {
            Ok(false)
        }
    }

    #[derive(Debug, Clone)]
    struct Recorded {
        method: String,
        target: String,
        authorization: Option<String>,
        body: Value,
    }

    struct MockEdge {
        url: String,
        requests: Arc<Mutex<Vec<Recorded>>>,
    }

    impl MockEdge {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = requests.clone();
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let log = log.clone();
                    tokio::spawn(async move {
                        let mut raw = Vec::new();
                        let mut chunk = [0u8; 8192];
                        let header_end = loop {
                            let Ok(n) = stream.read(&mut chunk).await else {
                                return;
                            };
                            if n == 0 {
                                return;
                            }
                            raw.extend_from_slice(&chunk[..n]);
                            if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                                break at + 4;
                            }
                        };
                        let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
                        let mut lines = head.lines();
                        let mut request_line = lines.next().unwrap_or("").split_whitespace();
                        let method = request_line.next().unwrap_or("").to_string();
                        let target = request_line.next().unwrap_or("").to_string();
                        let mut authorization = None;
                        let mut length = 0usize;
                        for line in lines {
                            let Some((name, value)) = line.split_once(':') else {
                                continue;
                            };
                            if name.eq_ignore_ascii_case("authorization") {
                                authorization = Some(value.trim().to_string());
                            }
                            if name.eq_ignore_ascii_case("content-length") {
                                length = value.trim().parse().unwrap_or(0);
                            }
                        }
                        while raw.len() < header_end + length {
                            let Ok(n) = stream.read(&mut chunk).await else {
                                return;
                            };
                            if n == 0 {
                                break;
                            }
                            raw.extend_from_slice(&chunk[..n]);
                        }
                        let body = serde_json::from_slice(&raw[header_end..header_end + length])
                            .unwrap_or(Value::Null);
                        log.lock().unwrap().push(Recorded {
                            method: method.clone(),
                            target: target.clone(),
                            authorization,
                            body: body.clone(),
                        });
                        let reply = route(&method, &target, &body);
                        let bytes = serde_json::to_vec(&reply).unwrap();
                        let head = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            bytes.len()
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(&bytes).await;
                    });
                }
            });
            Self { url, requests }
        }

        fn client(&self) -> CloudClient {
            CloudClient::new(
                self.url.clone(),
                "org",
                Arc::new(zeron_rpc::StaticToken("user-token".into())),
            )
        }

        fn requests(&self) -> Vec<Recorded> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn route(method: &str, target: &str, body: &Value) -> Value {
        match (method, target) {
            ("GET", "/cloud/org/sessions") => json!({
                "sessions": [{
                    "chatId": "cloud-chat",
                    "deviceId": "cloud-device",
                    "spaceId": "space",
                    "state": "ready",
                    "createdAt": 1
                }]
            }),
            ("GET", "/cloud/org/sessions/cloud-chat/schedules") => json!({
                "schedules": [{
                    "id": "edge-task",
                    "chat_id": "cloud-chat",
                    "title": "Edge",
                    "prompt": "edge prompt",
                    "rrule": "DTSTART:20990101T090000Z\nRRULE:FREQ=DAILY",
                    "active": 1,
                    "next_fire_at": 4070908800000i64,
                    "last_fired_at": null,
                    "last_error": null,
                    "created_at": 1,
                    "updated_at": 2
                }]
            }),
            ("POST", "/cloud/org/sessions/cloud-chat/schedules") => json!({
                "schedule": {
                    "id": body.get("id").and_then(Value::as_str).unwrap_or("edge-created"),
                    "chat_id": "cloud-chat",
                    "title": body["title"],
                    "prompt": body["prompt"],
                    "rrule": body["rrule"],
                    "active": if body["active"].as_bool().unwrap_or(true) { 1 } else { 0 },
                    "next_fire_at": 4070908800000i64,
                    "last_fired_at": null,
                    "last_error": null,
                    "created_at": 1,
                    "updated_at": 2
                }
            }),
            ("DELETE", "/cloud/org/sessions/cloud-chat/schedules/edge-task") => {
                json!({"deleted": true})
            }
            _ => json!({}),
        }
    }

    fn store() -> (tempfile::TempDir, Schedules) {
        let dir = tempfile::tempdir().unwrap();
        let schedules = Schedules::open(dir.path(), Arc::new(NoDeliver));
        (dir, schedules)
    }

    fn task(id: &str, chat: &str) -> Value {
        json!({
            "id": id,
            "title": "Task",
            "prompt": "prompt",
            "rrule": "DTSTART:20990101T090000Z\nRRULE:FREQ=DAILY",
            "target": {"kind": "session", "session": chat},
            "active": true
        })
    }

    #[tokio::test]
    async fn settings_list_merges_device_and_edge_rows() {
        let edge = MockEdge::start().await;
        let (_dir, schedules) = store();
        crate::schedules::call(
            &schedules,
            methods::SCHEDULE_SAVE,
            &json!({"task": task("", "local-chat")}),
        )
        .unwrap();

        let listed = call(&schedules, edge.client(), methods::SCHEDULES, &json!({}))
            .await
            .unwrap();
        let tasks = listed["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(
            tasks
                .iter()
                .any(|task| task["origin"].is_null() && task["target"]["session"] == "local-chat")
        );
        assert!(
            tasks
                .iter()
                .any(|task| task["origin"] == "edge" && task["target"]["session"] == "cloud-chat")
        );
    }

    #[tokio::test]
    async fn creating_a_cloud_chat_schedule_writes_the_edge_not_the_local_store() {
        let edge = MockEdge::start().await;
        let (dir, schedules) = store();
        let saved = call(
            &schedules,
            edge.client(),
            methods::SCHEDULE_SAVE,
            &json!({"task": task("", "cloud-chat")}),
        )
        .await
        .unwrap();
        assert_eq!(saved["id"], "edge-created");
        assert_eq!(saved["origin"], "edge");
        assert!(!dir.path().join("schedules.json").exists());
        assert!(edge.requests().iter().any(|request| {
            request.method == "POST"
                && request.target == "/cloud/org/sessions/cloud-chat/schedules"
                && request.authorization.as_deref() == Some("Bearer user-token")
                && request.body["prompt"] == "prompt"
        }));
    }

    #[tokio::test]
    async fn editing_an_edge_row_replaces_that_edge_row() {
        let edge = MockEdge::start().await;
        let (_dir, schedules) = store();
        let mut edited = task("edge-task", "cloud-chat");
        edited["title"] = json!("Paused edge");
        edited["active"] = json!(false);

        let saved = call(
            &schedules,
            edge.client(),
            methods::SCHEDULE_SAVE,
            &json!({"task": edited}),
        )
        .await
        .unwrap();

        assert_eq!(saved["id"], "edge-task");
        assert_eq!(saved["origin"], "edge");
        assert_eq!(saved["active"], false);
        assert!(edge.requests().iter().any(|request| {
            request.method == "POST"
                && request.body["id"] == "edge-task"
                && request.body["title"] == "Paused edge"
                && request.body["active"] == false
        }));
    }

    #[tokio::test]
    async fn deleting_and_running_an_edge_row_are_explicit() {
        let edge = MockEdge::start().await;
        let (_dir, schedules) = store();
        let deleted = call(
            &schedules,
            edge.client(),
            methods::SCHEDULE_DELETE,
            &json!({"id": "edge-task"}),
        )
        .await
        .unwrap();
        assert_eq!(deleted["deleted"], true);
        assert!(edge.requests().iter().any(|request| {
            request.method == "DELETE"
                && request.target == "/cloud/org/sessions/cloud-chat/schedules/edge-task"
        }));

        let error = call(
            &schedules,
            edge.client(),
            methods::SCHEDULE_RUN_NOW,
            &json!({"id": "edge-task"}),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Run now isn't available for edge-owned schedules")
        );
    }
}
