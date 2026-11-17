use std::collections::{HashMap, HashSet};

use serde_json::Value;
use zeron_proto::{AgentEvent, DoneStatus, ToolCall};

use crate::HarnessError;

pub(super) struct Normalizer {
    session: String,
    children: HashSet<String>,
    sequences: HashMap<String, u64>,
    pub error: Option<String>,
}

impl Normalizer {
    pub(super) fn new(session: String) -> Self {
        Self {
            session,
            children: HashSet::new(),
            sequences: HashMap::new(),
            error: None,
        }
    }

    pub(super) fn notification(&mut self, frame: &Value) -> Result<Vec<AgentEvent>, HarnessError> {
        let method = frame["method"].as_str().unwrap_or("");
        let params = &frame["params"];
        if method == "subagent.started" {
            let parent = required(params, "parentSessionId")?;
            let child = required(params, "childSessionId")?;
            if parent != self.session && !self.children.contains(parent) {
                return Ok(vec![]);
            }
            if !self.children.insert(child.into()) {
                return Ok(vec![]);
            }
            return Ok(vec![AgentEvent::ToolCall {
                id: child.into(),
                call: ToolCall::Unknown {
                    name: "Agent".into(),
                    input: None,
                },
            }]);
        }
        if method == "subagent.finished" {
            let child = required(params, "childSessionId")?;
            if !self.children.contains(child) {
                return Ok(vec![]);
            }
            let status = if params["status"] == "ok" {
                DoneStatus::Completed
            } else {
                DoneStatus::Errored
            };
            return Ok(vec![AgentEvent::Subagent {
                parent_tool_use_id: child.into(),
                event: Box::new(AgentEvent::Done {
                    status,
                    result: None,
                    error: (status == DoneStatus::Errored)
                        .then(|| "DeepSeek subagent failed".into()),
                    session_id: Some(child.into()),
                }),
            }]);
        }
        if method != "session.event" {
            return Ok(vec![]);
        }
        let session = required(params, "sessionId")?;
        if session != self.session && !self.children.contains(session) {
            return Ok(vec![]);
        }
        let event = &params["event"];
        if let Some(seq) = event["seq"].as_u64() {
            if self.sequences.get(session).is_some_and(|last| seq <= *last) {
                return Ok(vec![]);
            }
            self.sequences.insert(session.into(), seq);
        }
        let data = &event["data"];
        let mut result = Vec::new();
        match required(event, "type")? {
            "assistant/message" => {
                let mut recorded = false;
                if let Some(stream) = data["stream"].as_array() {
                    for record in stream {
                        let reasoning = match record["type"].as_str() {
                            Some("text-chunks") => false,
                            Some("reasoning-chunks") => true,
                            _ => continue,
                        };
                        let texts = record["texts"].as_array().ok_or_else(|| {
                            HarnessError::Protocol("Invalid SDK assistant stream".into())
                        })?;
                        for text in texts {
                            let text = text
                                .as_str()
                                .ok_or_else(|| {
                                    HarnessError::Protocol("Invalid SDK assistant delta".into())
                                })?
                                .to_owned();
                            result.push(if reasoning {
                                AgentEvent::ReasoningDelta { text }
                            } else {
                                AgentEvent::TextDelta { text }
                            });
                            recorded = true;
                        }
                    }
                }
                if !recorded {
                    let blocks = data["message"]["content"].as_array().ok_or_else(|| {
                        HarnessError::Protocol("Invalid SDK assistant content".into())
                    })?;
                    for block in blocks {
                        if let Some(text) = block["text"].as_str() {
                            match block["type"].as_str() {
                                Some("text") => {
                                    result.push(AgentEvent::TextDelta { text: text.into() })
                                }
                                Some("reasoning") => {
                                    result.push(AgentEvent::ReasoningDelta { text: text.into() })
                                }
                                _ => {}
                            }
                        }
                    }
                }
                if let Some(usage) = data.get("usage") {
                    if let (Some(input), Some(output)) = (
                        usage["inputTokens"].as_u64(),
                        usage["outputTokens"].as_u64(),
                    ) {
                        let input = input
                            .saturating_add(usage["cacheReadTokens"].as_u64().unwrap_or(0))
                            .saturating_add(usage["cacheWriteTokens"].as_u64().unwrap_or(0));
                        result.push(AgentEvent::Usage {
                            input_tokens: input,
                            output_tokens: output,
                        });
                        result.push(AgentEvent::ContextUsage {
                            tokens: Some(input),
                            window: None,
                        });
                    }
                }
                if let Some(id) = data["message"]["id"].as_str() {
                    result.push(AgentEvent::AssistantMessageCompleted {
                        assistant_message_id: id.into(),
                    });
                }
            }
            "tool/call" => {
                let id = required(data, "callId")?.to_owned();
                let name = required(data, "name")?;
                let arguments = data["arguments"].as_str().unwrap_or("{}");
                let input: Value = serde_json::from_str(arguments)
                    .unwrap_or_else(|_| serde_json::json!({"raw":arguments}));
                let path = input["file_path"].as_str().unwrap_or("").to_owned();
                let call = match name {
                    "bash" | "pwsh" => ToolCall::Exec {
                        command: input["command"].as_str().unwrap_or(arguments).into(),
                    },
                    "read" => ToolCall::ReadFile { path },
                    "write" => ToolCall::WriteFile {
                        path,
                        content: input["content"].as_str().map(str::to_owned),
                    },
                    "edit" => ToolCall::EditFile {
                        path,
                        old_string: input["old_string"].as_str().map(str::to_owned),
                        new_string: input["new_string"].as_str().map(str::to_owned),
                    },
                    "apply_patch" => ToolCall::ApplyPatch { path: None },
                    _ if name.starts_with("mcp__") => {
                        let (server, tool) = name
                            .trim_start_matches("mcp__")
                            .split_once("__")
                            .unwrap_or(("unknown", name));
                        ToolCall::Mcp {
                            server: server.into(),
                            tool: tool.into(),
                            input: Some(input),
                        }
                    }
                    _ => ToolCall::Unknown {
                        name: name.into(),
                        input: Some(input),
                    },
                };
                result.push(AgentEvent::ToolCall { id, call });
            }
            "tool/result" => {
                let message = &data["message"];
                let id = required(message, "toolCallId")?.to_owned();
                let output: String = message["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                            .chars()
                            .take(32768)
                            .collect()
                    })
                    .unwrap_or_default();
                result.push(AgentEvent::ToolResult {
                    id,
                    is_error: message["isError"].as_bool().unwrap_or(false),
                    output: (!output.is_empty()).then_some(output),
                    diff: None,
                    preview_paths: vec![],
                    image_payloads: vec![],
                    exit_code: None,
                });
            }
            "turn/end" if data["reason"]["kind"] == "error" => {
                let error = data["reason"]["error"]["message"]
                    .as_str()
                    .unwrap_or("DeepSeek turn failed")
                    .to_owned();
                if session == self.session {
                    self.error = Some(error.clone());
                }
                result.push(AgentEvent::Error { message: error });
            }
            "user/message" if session != self.session => {
                if let Some(blocks) = data["content"].as_array() {
                    result.push(AgentEvent::UserMessage {
                        text: blocks
                            .iter()
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    });
                }
            }
            _ => {}
        }
        if session != self.session {
            result = result
                .into_iter()
                .map(|event| AgentEvent::Subagent {
                    parent_tool_use_id: session.into(),
                    event: Box::new(event),
                })
                .collect();
        }
        Ok(result)
    }
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, HarnessError> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| HarnessError::Protocol(format!("DeepSeek SDK event lacks {key}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn committed_stream_preserves_order_and_rejects_duplicates() {
        let mut normalizer = Normalizer::new("root".into());
        let frame = json!({"method":"session.event","params":{"sessionId":"root","event":{"seq":1,"type":"assistant/message","data":{"message":{"id":"message","content":[]},"stream":[{"type":"reasoning-chunks","texts":["think"]},{"type":"text-chunks","texts":["a","b"]}]}}}});
        let events = normalizer.notification(&frame).unwrap();
        assert!(matches!(&events[0], AgentEvent::ReasoningDelta { text } if text == "think"));
        assert!(matches!(&events[1], AgentEvent::TextDelta { text } if text == "a"));
        assert!(matches!(&events[2], AgentEvent::TextDelta { text } if text == "b"));
        assert!(normalizer.notification(&frame).unwrap().is_empty());
    }

    #[test]
    fn malformed_consumed_event_fails_instead_of_reporting_success() {
        let mut normalizer = Normalizer::new("root".into());
        assert!(normalizer.notification(&json!({"method":"session.event","params":{"sessionId":"root","event":{"type":"tool/call","data":{}}}})).is_err());
    }
}
