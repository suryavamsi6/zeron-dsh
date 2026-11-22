use std::path::Path;

use serde::{Deserialize, Serialize};

/// The pinned parser, embedded so a shipped binary carries it.
const CHAT_EXPORT: &str = include_str!("chat-export.mjs");

/// One branch-selected turn from a DeepSeek v3 export, as `chat-export.mjs`
/// normalizes it. Text, thinking and links are already separated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedTurn {
    pub role: ExportedRole,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub thinking: String,
    #[serde(default)]
    pub links: Vec<ExportedLink>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportedRole {
    User,
    Assistant,
}

impl ExportedRole {
    fn as_str(self) -> &'static str {
        match self {
            ExportedRole::User => "user",
            ExportedRole::Assistant => "assistant",
        }
    }
}

impl PartialEq<str> for ExportedRole {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for ExportedRole {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportedLink {
    #[serde(default)]
    pub title: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedConversation {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub inserted_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub turns: Vec<ExportedTurn>,
}

/// The most conversations the Rust side asks the parser for. Passing a ceiling
/// above the parser's own default is what lets the engine count what an export
/// holds before it keeps the newest 1000; above this the count is unknowable.
pub const MAX_PARSED_CONVERSATIONS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatExport {
    pub conversations: Vec<ExportedConversation>,
    /// How many conversations the parser returned, or `None` when the export
    /// held at least `MAX_PARSED_CONVERSATIONS`.
    pub total: Option<usize>,
}

/// Read `zip` through the pinned v3 parser and return its normalized
/// conversations, in the parser's order.
pub fn read_chat_export(zip: &Path) -> Result<ChatExport, String> {
    let metadata = std::fs::metadata(zip)
        .map_err(|error| format!("cannot read {}: {error}", zip.display()))?;
    if metadata.len() > 256 * 1024 * 1024 {
        return Err("the export is larger than 256 MiB".into());
    }
    let node =
        super::node().ok_or_else(|| "node is required to read a DeepSeek chat export".to_string())?;
    let driver = format!(
        "{CHAT_EXPORT}\ntry {{\n  const __export = readExport(await readFile(process.argv[1]), {{ maxSessions: {} }});\n  \
         process.stdout.write(JSON.stringify(__export));\n}} catch (error) {{\n  \
         console.error(error?.message ?? String(error));\n  process.exitCode = 1;\n}}\n",
        MAX_PARSED_CONVERSATIONS
    );
    let output = std::process::Command::new(node)
        .arg("--input-type=module")
        .arg("-e")
        .arg(driver)
        .arg(zip)
        .output()
        .map_err(|error| format!("cannot run node: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("chat-export: {}", cap(stderr.trim(), 2048)));
    }
    let conversations = serde_json::from_slice::<Vec<ExportedConversation>>(&output.stdout)
        .map_err(|error| format!("cannot read the parser's output: {error}"))?;
    // Truncation to 1000 conversations happens inside the pinned parser.
    if conversations.iter().any(|c| c.id.trim().is_empty()) {
        return Err("the export has a conversation with no id".into());
    }
    // Hitting the ceiling means the export held at least this many; the exact
    // count is unknowable, so it is not reported as a number.
    let total = if conversations.len() < MAX_PARSED_CONVERSATIONS {
        Some(conversations.len())
    } else {
        None
    };
    Ok(ChatExport { conversations, total })
}

/// The first `max` bytes of `s`, not splitting a UTF-8 character.
fn cap(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
