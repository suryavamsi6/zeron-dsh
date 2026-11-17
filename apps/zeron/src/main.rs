//! zeron — the command line and the background engine. The app's window is
//! zgx (`apps/zgx`): this binary runs the engine alone and the commands around
//! it, and has no interface of its own.
//!
//! `zeron headless` runs the engine; `zeron login` and `zeron logout` select the
//! profile used by the next engine start without mutating a live runtime.
//!
//! The command line is `zeron_cli`'s (shared with the engine helper and the zgx
//! interface). This binary links the engine, so it runs the engine's
//! subcommands itself.

use zeron_cli::{Engines, Launch};

fn main() -> anyhow::Result<()> {
    match zeron_cli::run(zeron_update::current_version(), Engines::InProcess) {
        Launch::Done(result) => result,
        Launch::Engine(command) => zeron_app::run_engine_command(command),
        // No window here. The app is zgx, which owns the interface; this binary
        // is what its packaging ships beside it as the engine and the CLI.
        Launch::Window { .. } => anyhow::bail!(
            "this build has no window — the Zeron app is zgx. \
             Run `zeron headless` for the engine, or `zeron --help` for the commands."
        ),
        #[cfg(target_os = "linux")]
        Launch::Appshot => anyhow::bail!("this build has no window to take an Appshot of"),
    }
}
