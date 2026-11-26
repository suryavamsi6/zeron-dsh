use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use zeron_proto::HarnessId;

use crate::process::{Command, Stdio};
use crate::{CancellationToken, Harness, HarnessError};

/// npm resolves, downloads and links the SDK's tree; a slow mirror is not a
/// failure.
const DEADLINE: Duration = Duration::from_secs(15 * 60);

/// DeepSeek's SDK is an npm install into the data dir, so a device with Node
/// can install it in place; anything else gets the manual command.
pub fn can_install(id: HarnessId) -> bool {
    id == HarnessId::DeepSeek && prerequisites().is_some()
}

/// Node and its npm's cli entry, when both are really there and the runtime
/// can start what it installs: the SDK's shim refuses Node below 24.9, so an
/// older one is not a prerequisite — installing through it would "succeed"
/// into a harness that never runs.
fn prerequisites() -> Option<(PathBuf, PathBuf)> {
    let node = crate::deepseek::node()?;
    let version = crate::executable::node_version(&node)?;
    if version < (24, 9, 0) {
        return None;
    }
    let npm_cli = npm_cli_for(&node)?;
    Some((node, npm_cli))
}

/// npm's own entry point, resolved against the Node install it belongs to,
/// so `node npm-cli.js` needs no shell and no shim: Windows' `npm.cmd` is a
/// batch file no process spawn can run directly, and a Unix `npm` is a
/// symlink into the same file. Version managers (nvm, fnm, mise, the
/// official installers) all ship this layout; a distribution split
/// (Debian's /usr/share) falls back to the manual command.
fn npm_cli_for(node: &Path) -> Option<PathBuf> {
    let dir = node.parent()?;
    let unix = dir
        .parent()
        .map(|prefix| prefix.join("lib/node_modules/npm/bin/npm-cli.js"))
        .unwrap_or_else(|| dir.join("../lib/node_modules/npm/bin/npm-cli.js"));
    // Unix and version managers: <prefix>/lib/node_modules/npm with the
    // binary at <prefix>/bin/node. Windows: node.exe at the prefix root.
    let candidates = [unix, dir.join("node_modules/npm/bin/npm-cli.js")];
    candidates.into_iter().find(|candidate| candidate.is_file())
}

pub fn manual_command(id: HarnessId) -> Option<&'static str> {
    (id == HarnessId::DeepSeek).then_some(
        "Set ZERON_DEEPSEEK_SDK_DIR to an isolated npm prefix, then run: npm install --prefix \"$ZERON_DEEPSEEK_SDK_DIR\" --save-exact @deepseek-ai/dsh-sdk-client@0.2.2-rc.3 @deepseek-ai/dsh@0.2.2-rc.3 (Node >=24.9)",
    )
}

pub fn installed(id: HarnessId) -> bool {
    id == HarnessId::DeepSeek && crate::DeepSeekHarness::new().installed()
}

pub async fn install_harness(
    id: HarnessId,
    cancel: CancellationToken,
) -> Result<(), HarnessError> {
    if id != HarnessId::DeepSeek {
        return Err(HarnessError::Install(
            manual_command(id)
                .unwrap_or("This historical harness is no longer supported")
                .into(),
        ));
    }
    let (node, npm_cli) = prerequisites().ok_or_else(|| {
        HarnessError::NotInstalled(format!(
            "Node.js is required to install the DeepSeek SDK: {}",
            manual_command(id).unwrap()
        ))
    })?;
    let sdk_root = crate::DeepSeekHarness::new().sdk_root().to_path_buf();
    install_deepseek_with(&node, &npm_cli, &sdk_root, cancel).await
}

/// `npm install` the pinned SDK packages into `sdk_root`, as `node` running
/// npm's cli directly. Cancellation and the deadline both kill the child;
/// npm's own output tail carries the failure to the surface.
async fn install_deepseek_with(
    node: &Path,
    npm_cli: &Path,
    sdk_root: &Path,
    cancel: CancellationToken,
) -> Result<(), HarnessError> {
    std::fs::create_dir_all(sdk_root).map_err(|e| {
        HarnessError::Install(format!("cannot create {}: {e}", sdk_root.display()))
    })?;
    let version = crate::deepseek::SDK_VERSION;
    let mut command = Command::new(node);
    command
        .arg(npm_cli)
        .arg("install")
        .arg("--prefix")
        .arg(sdk_root)
        .arg("--save-exact")
        .arg("--no-audit")
        .arg("--no-fund")
        .arg(format!("@deepseek-ai/dsh-sdk-client@{version}"))
        .arg(format!("@deepseek-ai/dsh@{version}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::compose_child_path(&mut command, node);
    // npm's lifecycle scripts are its children's children: the child runs in
    // its own process group and shutdown ends the tree, so a cancelled or
    // timed-out install leaves no script running (Unix process groups).
    crate::process::owned::configure(&mut command);
    let mut child = crate::process::owned::Child::new(command.spawn().map_err(|e| {
        HarnessError::Install(format!("cannot start npm through {}: {e}", node.display()))
    })?);
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let running = &mut child;
    let collected = tokio::time::timeout(DEADLINE, async move {
        let (out, err) = tokio::join!(read_all(stdout), read_all(stderr));
        (out, err, running.wait().await)
    });
    let outcome = tokio::select! {
        collected = collected => Some(collected),
        _ = cancel.cancelled() => None,
    };
    let Some(collected) = outcome else {
        child.shutdown(Duration::from_secs(2)).await;
        return Err(HarnessError::Install("install cancelled".into()));
    };
    let (out, err, status) = match collected {
        Ok(done) => done,
        Err(_) => {
            child.shutdown(Duration::from_secs(2)).await;
            return Err(HarnessError::Install(format!(
                "npm install timed out after {} seconds",
                DEADLINE.as_secs()
            )));
        }
    };
    let status = status.map_err(|e| HarnessError::Install(format!("npm install: {e}")))?;
    if !status.success() {
        return Err(HarnessError::Install(tail(&output_text(&out, &err))));
    }
    if crate::deepseek::sdk_installed(sdk_root) {
        return Ok(());
    }
    Err(HarnessError::Install(format!(
        "npm finished but the SDK probe at {} still fails: {}",
        sdk_root.display(),
        tail(&output_text(&out, &err))
    )))
}

async fn read_all<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes).await;
    }
    bytes
}

fn output_text(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(stderr));
    text
}

const LOG_TAIL: usize = 600;

fn tail(log: &str) -> String {
    let start = log
        .char_indices()
        .rev()
        .nth(LOG_TAIL - 1)
        .map_or(0, |(at, _)| at);
    crate::redact_secrets(&log[start..])
}

#[cfg(feature = "installer-fixture")]
pub async fn install_with_command(
    _id: HarnessId,
    _command: &str,
    _cancel: CancellationToken,
) -> Result<(), HarnessError> {
    Err(HarnessError::Install(
        "DeepSeek SDK installation is manual".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake npm-cli.js: materializes the layout `sdk_installed` probes, so
    /// the install path runs end to end against the real Node when CI has
    /// one. Skipped silently where it does not.
    const FAKE_NPM: &str = r#"const fs = require('fs');
const path = require('path');
const at = process.argv.indexOf('--prefix');
const prefix = process.argv[at + 1];
for (const [pkg, entry] of [['dsh-sdk-client', 'lib/index.js'], ['dsh', 'lib/bin.js'], ['dsh-sdk-protocol', 'lib/index.js']]) {
  const dir = path.join(prefix, 'node_modules', '@deepseek-ai', pkg);
  fs.mkdirSync(path.join(dir, 'lib'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ version: '0.2.2-rc.3' }));
  fs.writeFileSync(path.join(dir, entry), '');
}
"#;

    fn write_fake_npm(dir: &Path) -> PathBuf {
        let cli = dir.join("npm-cli.js");
        std::fs::write(&cli, FAKE_NPM).unwrap();
        cli
    }

    #[tokio::test]
    async fn install_materializes_the_probed_layout() {
        let Some(node) = crate::deepseek::node() else { return };
        let dir = std::env::temp_dir().join(format!("zeron-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let npm_cli = write_fake_npm(&dir);
        let sdk_root = dir.join("sdk root with spaces");
        install_deepseek_with(&node, &npm_cli, &sdk_root, CancellationToken::new())
            .await
            .unwrap();
        assert!(crate::deepseek::sdk_installed(&sdk_root));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failing_npm_carries_its_own_output() {
        let Some(node) = crate::deepseek::node() else { return };
        let dir = std::env::temp_dir().join(format!("zeron-install-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("npm-cli.js");
        std::fs::write(&cli, "console.error('npm ERR! network boom'); process.exit(1)").unwrap();
        let error = install_deepseek_with(&node, &cli, &dir.join("sdk"), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(&error, HarnessError::Install(message) if message.contains("network boom")), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cancellation_kills_the_install() {
        let Some(node) = crate::deepseek::node() else { return };
        let dir = std::env::temp_dir().join(format!("zeron-install-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("npm-cli.js");
        std::fs::write(&cli, "setTimeout(() => {}, 60000)").unwrap();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            token.cancel();
        });
        let error = install_deepseek_with(&node, &cli, &dir.join("sdk"), cancel)
            .await
            .unwrap_err();
        assert!(matches!(&error, HarnessError::Install(message) if message.contains("cancelled")), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn npm_cli_resolution_is_the_prefix_layout() {
        let dir = std::env::temp_dir().join(format!("zeron-npm-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("prefix").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let node = bin.join("node");
        std::fs::write(&node, "").unwrap();
        assert!(npm_cli_for(&node).is_none());
        let cli = dir.join("prefix/lib/node_modules/npm/bin/npm-cli.js");
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, "").unwrap();
        assert_eq!(npm_cli_for(&node).as_deref(), Some(cli.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
