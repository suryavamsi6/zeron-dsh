//! schedule_mcp — the dsh-style schedule tools over the engine's own store.
//!
//! dsh's `tool-schedule` plugin speaks to a long-lived Host schedule service;
//! zeron's DeepSeek sidecar is per-run, so the engine owns the store
//! ([`crate::schedules`]) and this module is the bridge in front of it.
//!
//! Two halves, one contract. The child half ([`serve_stdio`]) is a `zeron`
//! binary launched as an extra MCP server for a DeepSeek run — marked
//! [`SCHEDULE_MCP_ENV`], it only relays: stdin frames out to the engine over
//! the agent-scoped IPC (`ScheduleMcp`), replies back. A child cannot share
//! the in-process [`Schedules`], so the engine answers every frame in-process
//! through [`ScheduleMcp`], the same store Settings → Automation tasks reads.
//!
//! Caller identity is bound here, never trusted from the wire: the engine
//! reads the caller's `Caller::Agent` chat (its handshake names the chat its
//! token was minted for) plus that chat's workspace row, producing a
//! [`Binding`] — the session `this_session` delivers to, the cwd `new_once` /
//! `new` chats open in, and whether the caller is a delegated subagent (which
//! dsh refuses outright). Mutating calls (`schedule_create`, `schedule_edit`)
//! reach the person through the run's existing approval path, like every
//! other non-read tool.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use zeron_rpc::{RpcError, methods};

use crate::schedules::{self, Schedules, Target, Task};

/// Marks a `zeron mcp` child as this chat's schedule bridge (`1`): it serves
/// the four schedule tools instead of Zeron's chat catalog.
pub const SCHEDULE_MCP_ENV: &str = "ZERON_SCHEDULE_MCP";

/// The MCP server name a DeepSeek run launches the bridge under.
pub const SERVER_NAME: &str = "zeron-schedule";

/// Who a schedule bridge speaks for, bound by the engine at call time —
/// the IPC caller's chat plus its workspace row. Nothing the model or the
/// child process sends can name another session, cwd, or agent kind.
pub struct Binding {
    /// The calling chat's id: `this_session` delivers here.
    pub session: String,
    /// The calling chat's working directory: `new_once` / `new` chats open
    /// here, as a Session does in dsh.
    pub cwd: String,
    /// The calling chat is a delegated child (`childKind: subagent`): dsh
    /// refuses scheduled tasks to delegated subagents, and so does this.
    pub subagent: bool,
}

/// The engine-side half of the schedule MCP server: one JSON-RPC frame in,
/// its reply (or `None` for a notification) out, in-process over
/// [`Schedules`]. The `ScheduleMcp` RPC handler wraps this; the tests drive
/// it directly, so the wire path and the tested path share every line.
pub struct ScheduleMcp {
    schedules: Schedules,
    binding: Binding,
}

impl ScheduleMcp {
    pub fn new(schedules: Schedules, binding: Binding) -> Self {
        Self { schedules, binding }
    }

    /// Answer one inbound MCP message. Requests (`id` present) always get a
    /// response frame; notifications and non-objects get none.
    pub fn handle(&self, request: &Value) -> Option<Value> {
        let Some(object) = request.as_object() else {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "expected a JSON-RPC object",
            ));
        };
        let Some(id) = object.get("id").cloned() else {
            return None; // a notification: nothing to answer
        };
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Some(error_response(id, INVALID_REQUEST, "missing method"));
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        Some(self.handle_request(id, method, params))
    }

    fn handle_request(&self, id: Value, method: &str, params: Value) -> Value {
        match method {
            "initialize" => {
                let requested = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(LATEST_PROTOCOL);
                let version = if PROTOCOL_VERSIONS.contains(&requested) {
                    requested
                } else {
                    LATEST_PROTOCOL
                };
                ok_response(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => ok_response(id, json!({})),
            "tools/list" => ok_response(id, json!({ "tools": tools() })),
            "tools/call" => self.call(id, params),
            // Optional capabilities not advertised: answer empty rather than
            // erroring so permissive clients do not log noise.
            "resources/list" => ok_response(id, json!({ "resources": [] })),
            "prompts/list" => ok_response(id, json!({ "prompts": [] })),
            other => error_response(id, METHOD_NOT_FOUND, &format!("unknown method: {other}")),
        }
    }

    /// `tools/call`: dsh returns its declared errors (`subagent_session`,
    /// `invalid_*`, `schedule_not_found`, `schedule_conflict`) as result
    /// payloads the model reads, so every outcome below is a result object —
    /// never a protocol error and never `isError`.
    fn call(&self, id: Value, params: Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error_response(id, INVALID_PARAMS, "tools/call needs a tool name");
        };
        if !TOOL_NAMES.contains(&name) {
            return error_response(id, INVALID_PARAMS, &format!("unknown tool: {name}"));
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let value = if self.binding.subagent {
            failed(
                "subagent_session",
                "A delegated subagent cannot use scheduled tasks.",
            )
        } else {
            match name {
                "schedule_create" => self.create(&arguments),
                "schedule_list" => Ok(self.list(&arguments)),
                "schedule_view" => Ok(self.view(&arguments)),
                "schedule_edit" => self.edit(&arguments),
                _ => unreachable!(),
            }
            .unwrap_or_else(|(code, message)| failed(&code, &message))
        };
        ok_response(
            id,
            json!({
                "content": [{ "type": "text", "text": value.to_string() }],
                "structuredContent": value,
                "isError": false,
            }),
        )
    }

    // ---- the four tools ----------------------------------------------------

    /// `schedule_create`: a new active task on `target`, resolved against the
    /// binding — `this_session` is the caller's own chat, `new_once` and
    /// `new` open in its cwd.
    fn create(&self, args: &Value) -> Result<Value, (String, String)> {
        let target = self.target(args.get("target"), "invalid_prompt")?;
        let task = json!({
            "title": args.get("title").cloned().unwrap_or(Value::Null),
            "prompt": args.get("prompt").cloned().unwrap_or(Value::Null),
            "rrule": args.get("rrule").cloned().unwrap_or(Value::Null),
            "target": target,
            "active": true,
        });
        self.save(&task).map(|task| model_task(&task))
    }

    /// `schedule_list`: `{items, shown, total, limit, offset}` — active tasks
    /// first, each ordered by `scheduledAt`, as dsh lists them.
    fn list(&self, args: &Value) -> Value {
        let mut tasks = self.schedules_list();
        if let Some(active) = args.get("active").and_then(Value::as_bool) {
            tasks.retain(|task| task.active == active);
        }
        tasks.sort_by_key(|task| (!task.active, task.next_at.unwrap_or(i64::MAX)));
        let limit = args
            .get("limit")
            .and_then(Value::as_i64)
            .unwrap_or(100)
            .clamp(1, 500) as usize;
        let offset = args
            .get("offset")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0) as usize;
        let total = tasks.len();
        let items: Vec<Value> = tasks
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|task| model_task(&task))
            .collect();
        json!({
            "items": items,
            "shown": items.len(),
            "total": total,
            "limit": limit,
            "offset": offset,
        })
    }

    /// `schedule_view`: one task by id, or the declared `schedule_not_found`.
    fn view(&self, args: &Value) -> Value {
        let Some(id) = args
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return failed("invalid_selector", "schedule_view takes an id.");
        };
        match self.find(id) {
            Some(task) => model_task(&task),
            None => json!({ "id": id, "code": "schedule_not_found" }),
        }
    }

    /// `schedule_edit`: update (merge the provided fields into the stored
    /// task — its id and, for `new_once`, the chat delivery resolved are kept)
    /// or delete. `expectedUpdatedAt` guards against a stale view of the task.
    fn edit(&self, args: &Value) -> Result<Value, (String, String)> {
        let bad = |message: &str| Err(("invalid_selector".to_owned(), message.to_owned()));
        let Some(id) = args
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return bad("schedule_edit takes an id.");
        };
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(task) = self.find(id) else {
            return Ok(json!({ "id": id, "code": "schedule_not_found" }));
        };
        if let Some(expected) = args.get("expectedUpdatedAt").and_then(Value::as_str)
            && iso(task.updated_at) != expected
        {
            return Ok(json!({ "id": id, "code": "schedule_conflict" }));
        }
        match action {
            "delete" => {
                if ["title", "prompt", "rrule", "target", "active"]
                    .iter()
                    .any(|field| args.get(field).is_some())
                {
                    return bad("delete accepts only id, action, and expectedUpdatedAt.");
                }
                let reply = schedules::call(
                    &self.schedules,
                    methods::SCHEDULE_DELETE,
                    &json!({ "id": id }),
                )
                .map_err(store_error)?;
                if reply["deleted"] == true {
                    Ok(json!({ "id": id, "deleted": true }))
                } else {
                    Ok(json!({ "id": id, "code": "schedule_not_found" }))
                }
            }
            "update" => {
                let mut edited = task;
                let mut changed = false;
                if let Some(title) = args.get("title") {
                    edited.title = title.as_str().unwrap_or_default().to_owned();
                    changed = true;
                }
                if let Some(prompt) = args.get("prompt") {
                    edited.prompt = prompt.as_str().unwrap_or_default().to_owned();
                    changed = true;
                }
                if let Some(rrule) = args.get("rrule") {
                    edited.rrule = rrule.as_str().unwrap_or_default().to_owned();
                    changed = true;
                }
                if let Some(target) = args.get("target") {
                    edited.target = self.target(Some(target), "invalid_selector")?;
                    changed = true;
                }
                if let Some(active) = args.get("active") {
                    edited.active = active.as_bool().unwrap_or_default();
                    changed = true;
                }
                if !changed {
                    return bad(
                        "update requires at least one of title, prompt, rrule, target, or active.",
                    );
                }
                let task = serde_json::to_value(&edited)
                    .map_err(|e| ("internal_error".to_owned(), e.to_string()))?;
                self.save(&task).map(|task| model_task(&task))
            }
            _ => bad("action must be update or delete."),
        }
    }

    // ---- against the store --------------------------------------------------

    /// The store's whole list, as `Schedules` answers `Schedules`.
    fn schedules_list(&self) -> Vec<Task> {
        schedules::call(&self.schedules, methods::SCHEDULES, &json!({}))
            .ok()
            .and_then(|value| serde_json::from_value(value["tasks"].clone()).ok())
            .unwrap_or_default()
    }

    fn find(&self, id: &str) -> Option<Task> {
        self.schedules_list().into_iter().find(|task| task.id == id)
    }

    /// `ScheduleSave`: the store validates title/prompt/rrule and recomputes
    /// `nextAt`; its errors are translated to the dsh codes the tools declare.
    fn save(&self, task: &Value) -> Result<Task, (String, String)> {
        let saved = schedules::call(
            &self.schedules,
            methods::SCHEDULE_SAVE,
            &json!({ "task": task }),
        )
        .map_err(store_error)?;
        serde_json::from_value(saved).map_err(|e| ("internal_error".to_owned(), e.to_string()))
    }

    /// A model target choice → the store's `Target`: `this_session` is the
    /// bound chat, `new_once` / `new` open chats in the bound cwd.
    fn target(&self, choice: Option<&Value>, code: &str) -> Result<Target, (String, String)> {
        match choice.and_then(Value::as_str) {
            Some("this_session") => Ok(Target::Session {
                session: self.binding.session.clone(),
            }),
            Some("new_once") => Ok(Target::NewOnce {
                cwd: self.binding.cwd.clone(),
                session: None,
            }),
            Some("new") => Ok(Target::New {
                cwd: self.binding.cwd.clone(),
                session: None,
            }),
            _ => Err((
                code.to_owned(),
                "target must be this_session, new_once, or new.".to_owned(),
            )),
        }
    }
}

/// dsh's declared-error shape: `{code, message}`.
fn failed(code: &str, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

/// A store error → the closest dsh error code. `BadParams` messages name the
/// field that failed; anything else stays an `internal_error` like dsh's
/// withheld failures.
fn store_error(error: RpcError) -> (String, String) {
    let message = match &error {
        RpcError::BadParams(message) | RpcError::Failed(message) => message.clone(),
        other => other.to_string(),
    };
    let code = match error {
        RpcError::BadParams(_) => {
            if message.contains("no occurrence") {
                "not_future"
            } else if message.contains("prompt") || message.contains("title") {
                "invalid_prompt"
            } else {
                "invalid_rule"
            }
        }
        _ => "internal_error",
    };
    (code.to_owned(), message)
}

/// The model-facing task (dsh's `modelTask`): the stored record plus
/// `scheduledAt`/`scheduledAtLocal`, its status, and `updatedAt` — the fields
/// `expectedUpdatedAt` and `schedule_edit` reasoning are built on.
fn model_task(task: &Task) -> Value {
    let (zone, rule_next) = schedules::rule_view(&task.rrule, Utc::now()).unwrap_or((0, None));
    // A paused task keeps a nominal next occurrence so list/view still say
    // when it would fire; an ended one has none left.
    let next = match task.active {
        true => task.next_at,
        false => rule_next.map(|t| t.timestamp_millis()),
    };
    let status = match task.active {
        true => "active",
        false if rule_next.is_some() => "paused",
        false => "ended",
    };
    let Task {
        id,
        title,
        prompt,
        rrule,
        target,
        updated_at,
        ..
    } = task;
    json!({
        "kind": "rrule",
        "id": id,
        "title": title,
        "prompt": prompt,
        "rrule": rrule,
        "scheduledAt": next.map(iso),
        "scheduledAtLocal": next.map(|ms| iso_local(ms, zone)),
        "target": model_target(target),
        "status": status,
        "updatedAt": iso(*updated_at),
    })
}

/// The model-facing target (dsh's TARGET_SCHEMA): `session` names its chat;
/// `new_once` carries the chat delivery already resolved for it.
fn model_target(target: &Target) -> Value {
    match target {
        Target::Session { session } => json!({ "kind": "session", "sessionId": session }),
        Target::NewOnce { cwd, session } => json!({
            "kind": "new_once",
            "create": { "cwd": cwd },
            "sessionId": session,
        }),
        Target::New { cwd, session } => json!({
            "kind": "new",
            "create": { "cwd": cwd },
            "sessionId": session,
        }),
    }
}

/// Epoch ms → RFC 3339 UTC (what `updatedAt` and `scheduledAt` carry).
fn iso(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Epoch ms → the same instant in the rule's fixed offset (`scheduledAtLocal`).
fn iso_local(ms: i64, zone_secs: i64) -> String {
    let zone = FixedOffset::east_opt(zone_secs as i32)
        .unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .with_timezone(&zone)
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

// ---- MCP plumbing -----------------------------------------------------------

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const LATEST_PROTOCOL: &str = "2025-06-18";

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const PARSE_ERROR: i64 = -32700;

const INSTRUCTIONS: &str = "\
Zeron scheduled tasks: at each occurrence of a task's rrule its prompt is delivered to the \
task's target chat as a scheduled message from the user. schedule_create makes a task; \
schedule_list and schedule_view read tasks (including tasks made from other sessions); \
schedule_edit updates, pauses, resumes, or deletes one by id. \
Only create or modify scheduled tasks at the user's explicit request — mutating calls ask \
for the person's approval wherever the chat's access mode asks for it — and prior explicit \
user authorization for a running scheduled task remains valid. \
Targets: this_session delivers to this chat; new_once delivers to one new chat opened at the \
first occurrence; new opens a fresh chat for every occurrence. New chats use this chat's \
working directory. A delegated subagent cannot use scheduled tasks.";

const TOOL_NAMES: [&str; 4] = [
    "schedule_create",
    "schedule_edit",
    "schedule_list",
    "schedule_view",
];

fn ok_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "schedule_create",
            "description": "Create a scheduled task. At each occurrence of rrule, prompt is \
                delivered to the target session as a scheduled message from the user. The result \
                includes the task id and the next occurrence as scheduledAt in UTC and \
                scheduledAtLocal in the zone of rrule. After downtime, only the latest missed \
                occurrence is delivered. Tasks are created or changed only with explicit user \
                authorization.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Task name of at most 120 characters in the user's language: action plus object. Omit timing, status, ids, and bracketed suffixes."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Instruction delivered at each occurrence."
                    },
                    "rrule": {
                        "type": "string",
                        "description": RRULE_HELP
                    },
                    "target": {
                        "type": "string",
                        "enum": ["this_session", "new_once", "new"],
                        "description": TARGET_HELP
                    }
                },
                "required": ["title", "prompt", "rrule", "target"]
            }
        }),
        json!({
            "name": "schedule_list",
            "description": "List scheduled tasks, including tasks created from other sessions. \
                Active tasks come first, ordered by scheduledAt.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "active": {
                        "type": "boolean",
                        "description": "true for active tasks only; false for paused and ended tasks only; omitted for all tasks."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Page size from 1 to 500; defaults to 100."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Number of tasks to skip; defaults to 0."
                    }
                }
            }
        }),
        json!({
            "name": "schedule_view",
            "description": "View one scheduled task by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Task id." }
                },
                "required": ["id"]
            }
        }),
        json!({
            "name": "schedule_edit",
            "description": "Update, pause, resume, or delete one scheduled task by id. Deleting \
                does not retract queued messages. Tasks are created or changed only with \
                explicit user authorization.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Task id." },
                    "action": {
                        "type": "string",
                        "enum": ["update", "delete"],
                        "description": "update applies the provided fields, at least one; delete removes the task."
                    },
                    "title": {
                        "type": "string",
                        "description": "Task name of at most 120 characters in the user's language: action plus object. Omit timing, status, ids, and bracketed suffixes."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "New instruction delivered at each occurrence."
                    },
                    "rrule": {
                        "type": "string",
                        "description": "New recurrence in the schedule_create rrule format."
                    },
                    "target": {
                        "type": "string",
                        "enum": ["this_session", "new_once", "new"],
                        "description": "Omit to retain the destination and its creation settings. Providing target replaces them; new_once reserves a different chat. New targets capture this chat's working directory. Choices are the same as schedule_create."
                    },
                    "active": {
                        "type": "boolean",
                        "description": "false pauses the task; true resumes only if its timing has a future occurrence. Otherwise provide a new rrule."
                    },
                    "expectedUpdatedAt": {
                        "type": "string",
                        "description": "updatedAt from the latest result that returned this task; if the task changed after that result, the result is schedule_conflict and nothing changes."
                    }
                },
                "required": ["id", "action"]
            }
        }),
    ]
}

/// The engine's own rrule subset (the store is the authority, and it is
/// narrower than dsh's): one DTSTART line, a newline, one RRULE line; FREQ of
/// MINUTELY|HOURLY|DAILY|WEEKLY plus INTERVAL, BYDAY, BYHOUR, BYMINUTE,
/// COUNT and UNTIL.
const RRULE_HELP: &str = "\
Supported RFC 5545 subset: one DTSTART line, a newline, and one RRULE line. \
DTSTART ends in Z for UTC or carries TZID with a fixed offset such as +05:30; named zones are \
refused. DTSTART may be in the past; delivery starts with the first occurrence after creation. \
RRULE is FREQ=MINUTELY, HOURLY, DAILY, or WEEKLY plus optional INTERVAL, BYDAY (MO..SU), \
BYHOUR, BYMINUTE, COUNT, and UNTIL. A one-time delivery is RRULE:FREQ=DAILY;COUNT=1. \
UNTIL is a UTC instant ending in Z. \
Example: \"DTSTART:20260901T090000Z\\nRRULE:FREQ=DAILY\"";

const TARGET_HELP: &str = "\
this_session delivers to this chat; new_once delivers to the same new chat every time, opened \
at the first delivery; new opens a different new chat for every delivery. New chats use this \
chat's working directory. Use this_session for reminders and for follow-ups to this \
conversation; use new_once or new for independent work, such as a recurring report, and \
whenever the user asks for a new chat.";

// ---- the child half: stdin/stdout ⇄ the engine's IPC -------------------------

/// The child-process half of the bridge (`zeron mcp` marked
/// [`SCHEDULE_MCP_ENV`]): newline-delimited MCP frames on stdin are forwarded
/// to `ScheduleMcp` on the engine's IPC and the answers written back — a relay
/// like `zeron mcp` itself, narrowed to the schedule surface. It holds no
/// store and learns nothing the handshake doesn't already say: the engine
/// binds the caller's chat from the agent token's `x-zeron-chat`.
pub async fn serve_stdio(port: u16, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let mut bridge = Bridge {
        port,
        data_dir,
        rpc: None,
    };
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match stdin.read_until(b'\n', &mut buf).await {
            Ok(0) => break, // the client closed stdin: we're done
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                tracing::warn!(error = %error, "schedule-mcp: stdin failed");
                break;
            }
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                write_line(
                    &mut stdout,
                    &error_response(Value::Null, PARSE_ERROR, &format!("parse error: {error}")),
                )
                .await?;
                continue;
            }
        };
        let wants_reply = frame.get("id").is_some();
        let reply = match bridge.call(&frame).await {
            Ok(response) if !response.is_null() => Some(response),
            Ok(_) => None, // a notification the engine answered with null
            Err(error) if wants_reply => Some(error_response(
                frame["id"].clone(),
                INVALID_PARAMS,
                &format!("schedule-mcp: {error}"),
            )),
            Err(_) => None,
        };
        if let Some(reply) = reply {
            write_line(&mut stdout, &reply).await?;
        }
    }
    Ok(())
}

async fn write_line(stdout: &mut tokio::io::Stdout, frame: &Value) -> anyhow::Result<()> {
    stdout.write_all(frame.to_string().as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

/// The IPC dial the child half keeps, reconnecting once when the engine
/// restarts underneath a long-lived run (the same dance `Zeron::call` does).
struct Bridge {
    port: u16,
    data_dir: Option<PathBuf>,
    rpc: Option<Arc<zeron_rpc::RpcClient>>,
}

impl Bridge {
    async fn client(&mut self) -> Result<Arc<zeron_rpc::RpcClient>, RpcError> {
        if let Some(client) = &self.rpc {
            return Ok(client.clone());
        }
        let client =
            Arc::new(zeron_rpc::connect_ipc_as_agent(self.port, self.data_dir.as_deref()).await?);
        self.rpc = Some(client.clone());
        Ok(client)
    }

    /// One MCP frame to `ScheduleMcp`; `Ok(null)` is the engine saying the
    /// frame was a notification.
    async fn call(&mut self, frame: &Value) -> Result<Value, RpcError> {
        let client = self.client().await?;
        match client.call(methods::SCHEDULE_MCP, frame.clone()).await {
            Err(RpcError::Closed) => {
                self.rpc = None;
                let client = self.client().await?;
                let value = client.call(methods::SCHEDULE_MCP, frame.clone()).await?;
                Ok(value["response"].clone())
            }
            other => other.map(|value| value["response"].clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedules::Deliver;

    /// Delivery goes nowhere in tests: the tools are exercised against the
    /// store, never a fired occurrence.
    struct Recorder;
    impl Deliver for Recorder {
        fn deliver(&self, _task: &Task) -> Result<String, crate::EngineError> {
            Ok("chat-x".into())
        }
    }

    fn store(dir: &tempfile::TempDir) -> Schedules {
        Schedules::open(dir.path(), Arc::new(Recorder))
    }

    fn binding() -> Binding {
        Binding {
            session: "chat-1".into(),
            cwd: "/repo".into(),
            subagent: false,
        }
    }

    fn mcp(schedules: &Schedules) -> ScheduleMcp {
        ScheduleMcp::new(schedules.clone(), binding())
    }

    /// A JSON-RPC request frame's result field, unwrapped.
    fn result(mcp: &ScheduleMcp, id: i64, method: &str, params: Value) -> Value {
        let reply = mcp
            .handle(&json!({
                "jsonrpc": "2.0", "id": id, "method": method, "params": params,
            }))
            .unwrap_or_else(|| panic!("{method} got no reply"));
        assert!(reply["error"].is_null(), "{method} failed: {reply}");
        reply["result"].clone()
    }

    /// `tools/call` → the tool's structured result (`{code, …}` on a declared
    /// failure), with the text copy checked along the way.
    fn call_tool(mcp: &ScheduleMcp, id: i64, name: &str, args: Value) -> Value {
        let reply = result(
            mcp,
            id,
            "tools/call",
            json!({ "name": name, "arguments": args }),
        );
        assert_eq!(reply["isError"], false, "{reply}");
        assert_eq!(reply["content"][0]["type"], "text", "{reply}");
        assert_eq!(
            reply["content"][0]["text"].as_str(),
            Some(reply["structuredContent"].to_string().as_str()),
            "content text carries the structured result: {reply}"
        );
        reply["structuredContent"].clone()
    }

    const DAILY: &str = "DTSTART:20260101T090000Z\nRRULE:FREQ=DAILY";

    fn create(mcp: &ScheduleMcp, target: &str) -> Value {
        call_tool(
            mcp,
            10,
            "schedule_create",
            json!({
                "title": "standup",
                "prompt": "post the standup notes",
                "rrule": DAILY,
                "target": target,
            }),
        )
    }

    #[test]
    fn initialize_lists_exactly_the_four_tools() {
        let dir = tempfile::tempdir().unwrap();
        let mcp = mcp(&store(&dir));
        let init = result(
            &mcp,
            1,
            "initialize",
            json!({ "protocolVersion": "2024-11-05" }),
        );
        assert_eq!(init["protocolVersion"], "2024-11-05");
        assert_eq!(init["serverInfo"]["name"], SERVER_NAME);
        let tools = result(&mcp, 2, "tools/list", json!({}))["tools"].clone();
        let mut names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "schedule_create",
                "schedule_edit",
                "schedule_list",
                "schedule_view"
            ],
            "{tools}"
        );
        for tool in tools.as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        }
        let create = &tools[0];
        assert_eq!(
            create["inputSchema"]["required"],
            json!(["title", "prompt", "rrule", "target"])
        );
        assert_eq!(
            create["inputSchema"]["properties"]["target"]["enum"],
            json!(["this_session", "new_once", "new"])
        );
        // A notification gets no reply.
        assert!(
            mcp.handle(&json!({
                "jsonrpc": "2.0", "method": "notifications/initialized",
            }))
            .is_none()
        );
    }

    #[test]
    fn create_list_view_edit_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let schedules = store(&dir);
        let mcp = mcp(&schedules);

        let created = create(&mcp, "this_session");
        assert_eq!(created["kind"], "rrule");
        let id = created["id"].as_str().unwrap().to_owned();
        assert!(!id.is_empty());
        assert_eq!(created["title"], "standup");
        assert_eq!(created["rrule"], DAILY, "the rrule round-trips verbatim");
        assert_eq!(created["status"], "active");
        assert!(created["scheduledAt"].as_str().is_some());
        assert!(created["scheduledAtLocal"].as_str().is_some());
        assert_eq!(
            created["target"],
            json!({ "kind": "session", "sessionId": "chat-1" })
        );

        // The engine store itself — the `Schedules` Settings → Automation
        // tasks reads — holds what the tool made.
        let stored = schedules::call(&schedules, methods::SCHEDULES, &json!({})).unwrap();
        let tasks = stored["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], json!(id));
        assert_eq!(tasks[0]["rrule"], json!(DAILY));

        let viewed = call_tool(&mcp, 11, "schedule_view", json!({ "id": id }));
        assert_eq!(viewed["id"], json!(id));
        assert_eq!(viewed["rrule"], DAILY);
        assert_eq!(viewed["updatedAt"], created["updatedAt"]);

        let page = call_tool(&mcp, 12, "schedule_list", json!({}));
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["limit"], 100);
        assert_eq!(page["offset"], 0);

        let edited = call_tool(
            &mcp,
            13,
            "schedule_edit",
            json!({
                "id": id, "action": "update",
                "title": "daily standup",
                "expectedUpdatedAt": created["updatedAt"],
            }),
        );
        assert_eq!(edited["id"], json!(id));
        assert_eq!(edited["title"], "daily standup");
        assert_eq!(edited["prompt"], "post the standup notes");

        let paused = call_tool(
            &mcp,
            14,
            "schedule_edit",
            json!({ "id": id, "action": "update", "active": false }),
        );
        assert_eq!(paused["status"], "paused");

        let conflict = call_tool(
            &mcp,
            15,
            "schedule_edit",
            json!({
                "id": id, "action": "update", "title": "stale",
                "expectedUpdatedAt": "2020-01-01T00:00:00.000Z",
            }),
        );
        assert_eq!(conflict, json!({ "id": id, "code": "schedule_conflict" }));

        let deleted = call_tool(
            &mcp,
            16,
            "schedule_edit",
            json!({ "id": id, "action": "delete" }),
        );
        assert_eq!(deleted, json!({ "id": id, "deleted": true }));
        let stored = schedules::call(&schedules, methods::SCHEDULES, &json!({})).unwrap();
        assert_eq!(stored["tasks"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn this_session_binds_the_calling_chat() {
        let dir = tempfile::tempdir().unwrap();
        let schedules = store(&dir);
        let mcp = ScheduleMcp::new(
            schedules.clone(),
            Binding {
                session: "chat-42".into(),
                cwd: "/repo".into(),
                subagent: false,
            },
        );
        let created = create(&mcp, "this_session");
        assert_eq!(
            created["target"],
            json!({ "kind": "session", "sessionId": "chat-42" })
        );
        let stored = schedules::call(&schedules, methods::SCHEDULES, &json!({})).unwrap();
        assert_eq!(
            stored["tasks"][0]["target"],
            json!({ "kind": "session", "session": "chat-42" })
        );
    }

    #[test]
    fn new_targets_take_the_calling_chats_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let schedules = store(&dir);
        let mcp = mcp(&schedules);
        for (choice, kind) in [("new_once", "new_once"), ("new", "new")] {
            let created = create(&mcp, choice);
            assert_eq!(created["target"]["kind"], json!(kind));
            assert_eq!(created["target"]["create"]["cwd"], "/repo");
            assert!(created["target"]["sessionId"].is_null());
        }
    }

    /// `new_once` opens its chat at the first delivery; an edit that doesn't
    /// retarget keeps the chat the runner already resolved for it.
    #[test]
    fn new_once_keeps_its_resolved_chat_through_edits() {
        let dir = tempfile::tempdir().unwrap();
        let schedules = store(&dir);
        let saved = schedules::call(
            &schedules,
            methods::SCHEDULE_SAVE,
            &json!({ "task": {
                "title": "weekly report", "prompt": "post it",
                "rrule": DAILY,
                "target": { "kind": "new_once", "cwd": "/repo", "session": "chat-9" },
            } }),
        )
        .unwrap();
        let id = saved["id"].as_str().unwrap().to_owned();
        let mcp = mcp(&schedules);
        let edited = call_tool(
            &mcp,
            17,
            "schedule_edit",
            json!({ "id": id, "action": "update", "title": "renamed" }),
        );
        assert_eq!(edited["id"], json!(id));
        assert_eq!(edited["title"], "renamed");
        assert_eq!(edited["target"]["sessionId"], "chat-9");
        let stored = schedules::call(&schedules, methods::SCHEDULES, &json!({})).unwrap();
        assert_eq!(
            stored["tasks"][0]["target"],
            json!({ "kind": "new_once", "cwd": "/repo", "session": "chat-9" })
        );
    }

    #[test]
    fn missing_ids_and_tasks_fail_declared() {
        let dir = tempfile::tempdir().unwrap();
        let mcp = mcp(&store(&dir));
        for (name, args) in [
            ("schedule_view", json!({})),
            ("schedule_edit", json!({ "action": "delete" })),
            ("schedule_edit", json!({ "action": "update" })),
        ] {
            let failed = call_tool(&mcp, 20, name, args);
            assert_eq!(failed["code"], "invalid_selector", "{failed}");
        }
        assert_eq!(
            call_tool(&mcp, 21, "schedule_view", json!({ "id": "nope" })),
            json!({ "id": "nope", "code": "schedule_not_found" })
        );
        assert_eq!(
            call_tool(
                &mcp,
                22,
                "schedule_edit",
                json!({ "id": "nope", "action": "delete" })
            ),
            json!({ "id": "nope", "code": "schedule_not_found" })
        );
        // An update without a field is refused before the store sees it.
        let created = create(&mcp, "new");
        let id = created["id"].as_str().unwrap();
        let failed = call_tool(
            &mcp,
            23,
            "schedule_edit",
            json!({ "id": id, "action": "update" }),
        );
        assert_eq!(failed["code"], "invalid_selector");
        // And delete that carries a patch field.
        let failed = call_tool(
            &mcp,
            24,
            "schedule_edit",
            json!({ "id": id, "action": "delete", "title": "x" }),
        );
        assert_eq!(failed["code"], "invalid_selector");
    }

    #[test]
    fn a_subagent_cannot_use_the_tools() {
        let dir = tempfile::tempdir().unwrap();
        let schedules = store(&dir);
        let mcp = ScheduleMcp::new(
            schedules.clone(),
            Binding {
                session: "child-1".into(),
                cwd: "/repo".into(),
                subagent: true,
            },
        );
        for (name, args) in [
            (
                "schedule_create",
                json!({ "title": "t", "prompt": "p", "rrule": DAILY, "target": "new" }),
            ),
            ("schedule_list", json!({})),
            ("schedule_view", json!({ "id": "t1" })),
            ("schedule_edit", json!({ "id": "t1", "action": "delete" })),
        ] {
            let refused = call_tool(&mcp, 30, name, args);
            assert_eq!(
                refused,
                json!({
                    "code": "subagent_session",
                    "message": "A delegated subagent cannot use scheduled tasks."
                }),
                "{name}"
            );
        }
        let stored = schedules::call(&schedules, methods::SCHEDULES, &json!({})).unwrap();
        assert_eq!(stored["tasks"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn bad_rrules_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mcp = mcp(&store(&dir));
        for (rrule, code) in [
            ("RRULE:FREQ=DAILY", "invalid_rule"),
            (
                "DTSTART:20260101T090000Z\nRRULE:FREQ=MONTHLY",
                "invalid_rule",
            ),
            (
                "DTSTART;TZID=America/New_York:20260101T090000\nRRULE:FREQ=DAILY",
                "invalid_rule",
            ),
            (
                "DTSTART:20200101T090000Z\nRRULE:FREQ=DAILY;COUNT=1",
                "not_future",
            ),
        ] {
            let failed = call_tool(
                &mcp,
                40,
                "schedule_create",
                json!({
                    "title": "t", "prompt": "p", "rrule": rrule, "target": "new",
                }),
            );
            assert_eq!(failed["code"], code, "{rrule}");
        }
    }

    #[test]
    fn paused_and_ended_tasks_read_right() {
        let dir = tempfile::tempdir().unwrap();
        let mcp = mcp(&store(&dir));
        let created = create(&mcp, "new");
        let id = created["id"].as_str().unwrap().to_owned();
        let paused = call_tool(
            &mcp,
            50,
            "schedule_edit",
            json!({ "id": id, "action": "update", "active": false }),
        );
        assert_eq!(paused["status"], "paused");
        // A paused task still reports its nominal next occurrence.
        assert!(paused["scheduledAt"].as_str().is_some());
        let listed = call_tool(&mcp, 51, "schedule_list", json!({ "active": false }));
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["items"][0]["id"], json!(id));
    }

    /// The IPC contract the child half speaks: `ScheduleMcp` answers the
    /// whole inbound frame — here the same calls the tests drive directly,
    /// round-tripped through the method's params/reply shape.
    #[test]
    fn frames_round_trip_through_the_rpc_shape() {
        let dir = tempfile::tempdir().unwrap();
        let schedules = store(&dir);
        let mcp = mcp(&schedules);
        let frame = json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": { "name": "schedule_view", "arguments": { "id": "nope" } },
        });
        let reply = mcp.handle(&frame).unwrap();
        assert_eq!(reply["id"], 7);
        assert_eq!(
            reply["result"]["structuredContent"],
            json!({ "id": "nope", "code": "schedule_not_found" })
        );
    }
}
