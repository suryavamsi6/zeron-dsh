//! The person's own stdio MCP servers, one file on this device: Settings →
//! MCP servers lists them, and every run gets them beside Zeron's own server
//! and the plugins' (the DeepSeek driver writes each as an MCP client).
//!
//! The file is the whole list, `{"servers":[{name,command,args,env}]}` — the
//! same payload the settings page's save sends — written atomically and
//! owner-only. A missing file is no servers; an unreadable one is no servers
//! and a warning that carries no `env` value out (a token often lives there).
//! Entries a hand edit made unusable (a name the driver would refuse, a
//! missing command, a duplicate) are left out instead of ending up in a run.

use std::io::Write;
use std::path::{Path, PathBuf};

use zeron_proto::McpServer;

/// The most servers one device may keep: each is a program the driver starts
/// with a run, so the list stays short enough to start within its budget.
pub const MAX_SERVERS: usize = 16;
/// The file's name in the engine's data dir.
const FILE: &str = "mcp-servers.json";
/// The shim's insert ids: `[A-Za-z0-9_-]{1,32}`.
const MAX_NAME: usize = 32;

/// Where this device's servers are kept.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// The saved servers a run may start: the file's usable entries, at most
/// [`MAX_SERVERS`]. Never fails — an unreadable file is no servers.
pub fn load(data_dir: &Path) -> Vec<McpServer> {
    let text = match std::fs::read_to_string(path(data_dir)) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "mcp servers: the file can't be read; running without the person's servers"
            );
            return Vec::new();
        }
    };
    match serde_json::from_str::<Stored>(&text) {
        Ok(stored) => keep_usable(stored.servers),
        Err(err) => {
            // (The error says where, never what: the text holds env values.)
            tracing::warn!(
                line = err.line(),
                column = err.column(),
                "mcp servers: the file isn't valid JSON; running without the person's servers"
            );
            Vec::new()
        }
    }
}

/// Replace this device's list: the whole file, owner-only, atomically.
// (The settings page's save reaches it through SUR-73's SetMcpServers.)
#[allow(dead_code)]
pub fn save(data_dir: &Path, servers: &[McpServer]) -> std::io::Result<()> {
    let payload = serde_json::to_vec_pretty(&Stored {
        servers: servers.to_vec(),
    })
    .map_err(std::io::Error::other)?;
    let path = path(data_dir);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(
        ".{FILE}.tmp-{}-{}",
        std::process::id(),
        crate::new_id()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&payload)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    } else {
        // A crash after the rename must not lose the save: the directory's
        // own sync makes the rename durable where the platform allows it.
        #[cfg(unix)]
        if let Ok(directory) = std::fs::File::open(dir) {
            let _ = directory.sync_all();
        }
    }
    result
}

/// Why a list can't be saved, in words the settings page shows beside its
/// form: the driver's name rule, unique names, a command, and the ceiling.
#[allow(dead_code)]
pub fn validate(servers: &[McpServer]) -> Result<(), String> {
    if servers.len() > MAX_SERVERS {
        return Err(format!("At most {MAX_SERVERS} MCP servers"));
    }
    let mut seen = std::collections::HashSet::new();
    for server in servers {
        validate_one(server)?;
        if !seen.insert(server.name.as_str()) {
            return Err(format!("Two servers are named \"{}\"", server.name));
        }
    }
    Ok(())
}

/// The servers to add to a run: `existing` (Zeron's own, the plugins', the
/// schedule bridge) keeps its names, and a later duplicate of an earlier
/// user server is dropped — the driver refuses two servers with one name.
pub fn added(existing: &[McpServer], user: &[McpServer]) -> Vec<McpServer> {
    let mut taken: std::collections::HashSet<&str> =
        existing.iter().map(|server| server.name.as_str()).collect();
    let mut out = Vec::new();
    for server in user {
        if taken.insert(server.name.as_str()) {
            out.push(server.clone());
        }
    }
    out
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    #[serde(default)]
    servers: Vec<McpServer>,
}

fn keep_usable(servers: Vec<McpServer>) -> Vec<McpServer> {
    let mut out: Vec<McpServer> = Vec::new();
    let mut left_out = 0usize;
    for server in servers {
        // (This file is stdio: an HTTP entry is the built-in server's shape,
        // not one the settings page can have written.)
        let stdio = server.http.is_none();
        let usable = stdio
            && out.len() < MAX_SERVERS
            && validate_one(&server).is_ok()
            && !out.iter().any(|kept| kept.name == server.name);
        if !usable {
            left_out += 1;
            continue;
        }
        out.push(server);
    }
    if left_out > 0 {
        tracing::warn!(
            left_out,
            "mcp servers: entries the driver would refuse were left out"
        );
    }
    out
}

fn validate_one(server: &McpServer) -> Result<(), String> {
    let name = server.name.as_str();
    let well_formed = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !well_formed {
        return Err(format!(
            "\"{name}\" isn't a server name the driver accepts: letters, digits, - and _ only, up to {MAX_NAME}"
        ));
    }
    if server.command.trim().is_empty() {
        return Err(format!("\"{name}\" needs a command"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, command: &str) -> McpServer {
        McpServer {
            name: name.to_owned(),
            command: command.to_owned(),
            ..McpServer::default()
        }
    }

    fn names(servers: &[McpServer]) -> Vec<&str> {
        servers.iter().map(|server| server.name.as_str()).collect()
    }

    #[test]
    fn the_file_lives_beside_the_engine_s_own() {
        assert_eq!(
            path(Path::new("/data")),
            Path::new("/data").join("mcp-servers.json")
        );
    }

    #[test]
    fn no_file_is_no_servers() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_empty());
    }

    #[test]
    fn save_then_load_round_trips_every_field() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = server("files", "npx");
        first.args = vec!["-y".into(), "server-filesystem".into()];
        first.env.insert("NOTES".into(), "a value".into());
        let second = server("search_it", "/usr/bin/search");
        save(dir.path(), &[first.clone(), second.clone()]).unwrap();
        assert_eq!(load(dir.path()), vec![first, second]);
    }

    #[test]
    fn the_file_is_owner_folded_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &[server("a", "x")]).unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from(FILE)]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "only its owner reads a token there");
        }
    }

    #[test]
    fn a_corrupt_file_is_no_servers_not_a_failed_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "{not json").unwrap();
        assert!(load(dir.path()).is_empty());
        std::fs::write(path(dir.path()), r#"{"servers": 3}"#).unwrap();
        assert!(load(dir.path()).is_empty());
    }

    #[test]
    fn entries_the_driver_would_refuse_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut duplicated = server("b", "x");
        duplicated.command = "second".into();
        let mut http = server("remote", "x");
        http.http = Some(zeron_proto::McpHttp::default());
        let stored = Stored {
            servers: vec![
                server("ok-one", "x"),
                server("bad name", "x"),
                server("", "x"),
                server("no-command", "  "),
                duplicated,
                server("b", "x"),
                http,
            ],
        };
        std::fs::write(path(dir.path()), serde_json::to_vec(&stored).unwrap()).unwrap();
        let loaded = load(dir.path());
        assert_eq!(names(&loaded), vec!["ok-one", "b"]);
        assert_eq!(loaded[1].command, "second", "the first of two wins");
    }

    #[test]
    fn more_than_sixteen_saved_servers_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let many: Vec<McpServer> = (0..MAX_SERVERS + 4)
            .map(|i| server(&format!("s{i}"), "x"))
            .collect();
        std::fs::write(
            path(dir.path()),
            serde_json::to_vec(&Stored { servers: many }).unwrap(),
        )
        .unwrap();
        assert_eq!(load(dir.path()).len(), MAX_SERVERS);
    }

    #[test]
    fn validate_accepts_a_good_list_up_to_the_cap() {
        let good: Vec<McpServer> = (0..MAX_SERVERS)
            .map(|i| server(&format!("server-{i}_x"), "x"))
            .collect();
        assert_eq!(validate(&good), Ok(()));
        assert_eq!(validate(&[]), Ok(()));
    }

    #[test]
    fn validate_rejects_names_the_driver_would() {
        for name in ["", "has space", "colon:no", "a/b"] {
            assert!(
                validate(&[server(name, "x")]).is_err(),
                "{name:?} must be refused"
            );
        }
        let long = "a".repeat(MAX_NAME + 1);
        assert!(validate(&[server(&long, "x")]).is_err());
        let longest = "a".repeat(MAX_NAME);
        assert_eq!(validate(&[server(&longest, "x")]), Ok(()));
    }

    #[test]
    fn validate_rejects_a_duplicate_name() {
        let error = validate(&[server("same", "x"), server("same", "y")]).unwrap_err();
        assert!(error.contains("\"same\""), "{error}");
    }

    #[test]
    fn validate_rejects_an_empty_command() {
        assert!(validate(&[server("a", "")]).is_err());
        assert!(validate(&[server("a", "  ")]).is_err());
    }

    #[test]
    fn validate_rejects_more_than_sixteen() {
        let many: Vec<McpServer> = (0..MAX_SERVERS + 1)
            .map(|i| server(&format!("s{i}"), "x"))
            .collect();
        assert!(validate(&many).unwrap_err().contains("16"));
    }

    #[test]
    fn added_keeps_the_engine_s_own_names_and_the_user_s_order() {
        // Every source the stamp chains into `existing`: Zeron's own server,
        // a plugin's native server, the schedule bridge, and an earlier user
        // entry (the driver refuses two servers with one name).
        let existing = vec![
            server("zeron", "x"),
            server("plugin-native", "y"),
            server(crate::schedule_mcp::SERVER_NAME, "z"),
        ];
        let user = vec![
            server("mine", "a"),
            server("zeron", "shadow"),
            server("plugin-native", "shadow"),
            server(crate::schedule_mcp::SERVER_NAME, "shadow"),
            server("mine", "again"),
            server("second", "b"),
        ];
        assert_eq!(names(&added(&existing, &user)), vec!["mine", "second"]);
    }

    #[test]
    fn added_with_nothing_taken_is_the_whole_list() {
        let user = vec![server("a", "x"), server("b", "y")];
        assert_eq!(added(&[], &user), user);
    }

    /// The corrupt-file warning must name where, never what: a token often
    /// lives in an `env` value, and a log line is readable by anyone.
    #[test]
    fn a_corrupt_file_s_warning_carries_no_env_value() {
        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        // Truncated on purpose: valid so far, the entry's own env included.
        std::fs::write(
            path(dir.path()),
            r#"{"servers":[{"name":"a","command":"x","env":{"TOKEN":"secret-token-value"}}"#,
        )
        .unwrap();
        let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = {
            let logs = logs.clone();
            move || Capture(logs.clone())
        };
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .with_writer(writer)
            .finish();
        let loaded = tracing::subscriber::with_default(subscriber, || load(dir.path()));
        assert!(loaded.is_empty(), "a corrupt file is no servers");
        let text = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains("mcp servers"),
            "the warning was written: {text}"
        );
        assert!(
            !text.contains("secret-token-value"),
            "no env value may reach a log line: {text}"
        );
    }

    #[test]
    fn saved_servers_validate_as_saved() {
        let dir = tempfile::tempdir().unwrap();
        let list = vec![server("a", "x"), server("b", "y")];
        validate(&list).unwrap();
        save(dir.path(), &list).unwrap();
        assert_eq!(validate(&load(dir.path())), Ok(()));
    }
}
