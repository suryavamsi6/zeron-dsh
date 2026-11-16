# Zeron MCP server

`zeron mcp` serves the Model Context Protocol on stdin/stdout and proxies every
tool into the running engine's localhost IPC (`ws://127.0.0.1:$ZERON_IPC_PORT`,
default 27654) — the same `zeron_rpc` surface the headed app and `zeron sync`
dial. It is a subcommand of the one `zeron` binary: no Node runtime, no extra
install, a few MB resident.

Crate: `crates/mcp` (`zeron-mcp`). The protocol layer is hand-rolled
(`initialize`, `ping`, `tools/list`, `tools/call`; newline-delimited JSON-RPC
2.0) — the repo already owns JSON-RPC framing for the DeepSeek SDK bridge and
the stdio tool-server subset is tiny, so no SDK dependency was taken.

## Identity

The engine injects this server into the DeepSeek Harness SDK run, with the originating chat in the environment:

| Variable                | Meaning                                                  |
| ----------------------- | -------------------------------------------------------- |
| `ZERON_IPC_PORT`        | Engine to proxy (default 27654).                         |
| `ZERON_IPC_TOKEN_FILE`  | The engine's IPC bearer file (see below).                |
| `ZERON_CHAT_ID`         | The chat whose agent spawned this server.                |
| `ZERON_DEVICE_ID`       | That chat's host device.                                 |
| `ZERON_SIDE_CHATS_ONLY` | `1`: the agent has no subagent tools of its own (below). |

`ZERON_SIDE_CHATS_ONLY=1`: the host device turned native subagents off (Settings → DeepSeek), so `initialize` tells the agent to delegate with `create_chats`. Never set for a chat that has a parent, nor for one whose host doesn't know its parent yet.

### IPC authentication

The IPC port is loopback-only, but loopback is not an identity: any local
process could otherwise drive the engine (start agents, broker Cloud vault
grants). Every engine start that binds the port mints a fresh 32-byte token
and writes it to `{data_dir}/ipc-token` (owner-only `0600`, atomic replace;
on Windows the file inherits `%LOCALAPPDATA%\Zeron`'s per-user ACL). The
WebSocket handshake must carry `Authorization: Bearer <token>` (compared in
constant time) and is refused with `401` otherwise; the browser `Origin`
rejection (`403`) still applies on top. In-process (in-memory) transports are
unaffected.

Clients resolve the token at every dial — `$ZERON_IPC_TOKEN`, then the file
named by `$ZERON_IPC_TOKEN_FILE`, then `{ZERON_DATA_DIR or ~/.zeron}/ipc-token`
(`zeron_rpc::connect_ipc`) — so they follow the rotation across engine
restarts. The engine hands injected servers the file PATH (`ZERON_IPC_TOKEN_FILE`), never the token itself. The DeepSeek SDK receives each server's configuration through a private environment variable (`ZERON_DEEPSEEK_MCP_SECRET_<index>`), not through argv that other local users could read with `ps`. A client run against a different data dir (e.g. a viewport on a daemon's port) sets `ZERON_IPC_TOKEN_FILE` to the daemon's file.

When `ZERON_CHAT_ID` is set, every `send_message` is prefixed with a
`[Message from Zeron chat <title> (<id8>) …]` line so the receiving agent and the
human reading that transcript can tell an agent-to-agent message from a typed
one, and the server refuses to message its own chat. The transcript renders
this routing header as “Message from **chat name**”, keeping the full routing
instructions in the stored prompt for agents.

### Parent links

`create_chat` and each request in `create_chats` accept `kind`:

| Input | Placement |
| --- | --- |
| `kind: "chat"` | Standalone session, visible in the left **Sessions** sidebar; no `parentChatId` is written. |
| `kind: "side"` | Child of the explicit `parent` or origin chat. Requires one of those. |
| Kind omitted, origin or parent supplied | Child, preserving the previous default. |
| Kind omitted, no origin or parent | Standalone, preserving terminal usage. |

`kind: "chat"` with a nonempty `parent`, `kind: "side"` without an origin/parent,
and unknown kinds fail before any writes. Empty `parent` uses the default.
The creation result includes the effective `kind`, `chatId`, `deviceId`,
`project` and `parentChatId`. Kind is derived from the parent; it is not persisted.

Children record `Chat.parent_chat_id` (proto) ⇄ `parentChatId` on the registry
row (`Mutate createChat { parentChatId? }`). Any chat may delegate to a new DeepSeek chat.
Delegation is one level deep: a chat that has a parent creates no chats at all,
and a new chat's parent (the origin, or an explicit `parent`) must be top-level.
Standalone sessions have no parent.

`cloud: true` runs the child on a Cloud machine of its own (DeepSeek), cloning the project's GitHub repository; from a Cloud chat, the parent's
repository. Without it, a child of a Cloud chat shares the parent's machine. A
Cloud machine starts children only for the chat it runs, and sees and manages
only those children (docs/design/cloud-device.md, "Delegation").

For children, `parent` (id, prefix, or title) overrides the origin (`ZERON_CHAT_ID`).
`list_chats { parent }` returns children, and chat summaries carry `parentChatId`.
Rows from older engines read as parentless; a dangling parent id is tolerated.

The left sidebar hides chats that have a parent: `AppState::visible_chats`
(the Sessions list, project tabs, jump slots) and the Archived section both
require `parent_chat_id == None`. Children remain addressable by id, deep
link, and every MCP tool; `list_chats { parent }` is how an orchestrator
finds them.

### Injection

The host engine stamps this server onto every DeepSeek run it drives
(`RunRequest.mcp`, additive): the same `zeron` binary with `args: ["mcp"]`
and the variables above, pointed at the port the engine itself serves (never
a port it lost the bind race for). The DeepSeek SDK driver (Sidecar in
`crates/harness/src/deepseek/`) writes the run configuration
into `cordis.patch.yml` and inserts one [`@deepseek-ai/dsh-mcp-client`] plugin
per server:

```yaml
- insert:
    id: zeron-mcp-0
    name: '@deepseek-ai/dsh-mcp-client'
    config: !!js JSON.parse(process.env.ZERON_DEEPSEEK_MCP_SECRET_0)
```

The variable holds the stdio server description (`serverName`, `command`,
`args`, `env`, `cwd`, `failOnStartupError`, `startupTimeoutMs`). Server names
must match `[A-Za-z0-9_-]{1,32}` and be unique. Zeron's runtime disables the
SDK's own plugin manager, so no user-level plugin or MCP configuration from
the isolated Harness home is loaded on a run; Zeron's injected server is the only
MCP entry the host adds to a run.

Title runs never carry it. A run with no served port (embedded engine that
lost the bind) gets no Zeron tools rather than a dead server. A missing
Node >=24.9 or the pinned SDK fails the run as `NotInstalled` before any
MCP server starts.

### Forks and the session card

`ForkSideChat { chatId, sourceChatId, parentChatId? }` copies a chat's
history through its latest completed response into a new chat and appends
a `fork` part (a system entry: `sourceChatId`, `sourceTitle`) as the seam;
the transcript draws it as "This chat was forked from <title>". The copy
settles what only the source's runtime could finish: open questions copy
answered, and a spawn chip still `running` in the source copies `failed`
(its `subagentRef` intact). `parentChatId` defaults to the source; a side
chat's own fork button passes its parent so the copy lists as a sibling. The lanes card, under the session card, draws
a chat as a main line with its **side chats** (its children: forks and
`kind: "side"` spawns) and **subagents** (its spawn chips) branching off it,
and opens either in the right pane.

## Tools

Chat status uses the same shared derivation as the sidebar, including OSC 7501
reports from chat-owned terminals. `status` is `working`, `awaitingInput`,
`completed` (an unseen result), `errored`, or `idle`; `sessionStatus` retains the
native execution state. `statusStale` distinguishes expired live reports, and
`terminalActivity` identifies a terminal's input reason or result. Native waits
can return `interrupted`; a lost host is never interpreted as successful
completion. See [agent and terminal status](program-status.md).

Chats are referenced by full id, a unique id prefix, or an exact title.
Projects by id, path, display name, or unique path suffix. Devices by id or
name (default: the local engine's device).

| Tool               | Engine calls                                              |
| ------------------ | --------------------------------------------------------- |
| `whoami`           | `LocalDevice`, `EngineInfo`, origin chat summary          |
| `list_devices`     | `WatchDevices` snapshot                                   |
| `list_projects`    | `WatchSpaces` snapshot                                    |
| `list_harnesses`   | `ListHarnesses` + `ListAgentAccounts` (`targetDeviceId?`) |
| `list_models`      | `ListModels {harness, targetDeviceId?}`                                    |
| `list_chats`       | `WatchChats` + `WatchSessions` snapshots (status merged)  |
| `get_chat`         | above + `WatchDocMessages` opening frame (pending input)  |
| `create_chat`      | `Mutate createChat` (+ `renameChat`; optional first send) |
| `create_chats`     | Concurrent `create_chat` requests with per-request results |
| `send_messages`    | Concurrent `send_message` requests with per-request results |
| `read_chat`        | `WatchDocMessages` opening `reset` frame, rendered        |
| `send_message`     | `QueueCommand` Run / Steer, or `QueueMessage`             |
| `wait_for_turn`    | `WatchSessions` until the chat settles                    |
| `interrupt_chat`   | `QueueCommand` Interrupt                                  |
| `respond_to_input` | `QueueCommand` RespondInput                               |
| `archive_chat`     | `Mutate setChatArchived`                                  |
| `show_visual`      | `PublishVisual {chatId, title, script, assets}` (origin chat only; assets base64 by name, read by the MCP server from the agent's cwd) |
| `show_image`       | `PublishImage {chatId, name, data}` (origin chat only; the file base64, read by the MCP server from the agent's cwd) |

Watch streams are the engine's only read surface (there is no one-shot "get
transcript" RPC); a snapshot is "subscribe, take the first item, drop" — drop
cancels server-side, exactly what the sidebar does on attach.

`send_message` mode `auto` starts idle chats and steers working chats through
their live mailbox without interrupting tools or child processes. Providers
consume it at their next supported input boundary. Only explicit `queue` mode
creates a held queue row. `awaitingInput` refuses and points at
`respond_to_input`. Quiet, long-running turns still receive steering; the host
falls back to starting a turn if the live runtime has already exited.

DeepSeek Harness accepts steering at a turn boundary: the send is queued to
the SDK session and starts after the current turn settles. A steer that changes
`model`, `reasoning`, `access`, `sandbox`, `cwd`, or model options is kept out
of the live run; the composer labels the agent **Send next**. There is no
mid-turn cancel RPC in the official SDK: Stop closes and reaps Zeron's owned
sidecar process, and the next run resumes through persisted `session/resume`.

`wait_for_turn` after a send is edge-triggered on the `Session` row captured
before the send: it returns on a new `last_completed_turn`, an
`awaitingInput`/`errored` stamp newer than the baseline, or a working→idle
edge. A brand-new chat has no session row until the host picks the run up, so
the wait keeps waiting in that case rather than reporting the unstarted run as
done (this was the one bug the first live run found).

### Visual replies

`show_visual` is listed only when `ZERON_CHAT_ID` is set: it shows an
interactive visual (chart, table, dashboard, diagram, simulation, 3D view)
inline in the calling chat, above the agent's final reply. A visual is a Luau
script, not a web page. The script builds its data and state once and returns a
view function; the view returns a tree of `ui.*` nodes (rows, columns, grids,
text, stats, tables, charts, tabs, toggles, sliders, buttons, links, 2D
canvases, 3D scenes, and GPU shaders). Controls call the script's handlers, and Zeron renders
the view again.

`crates/visual` (`zeron-visual`) is the runtime. Scripts run in a Luau sandbox
with only the `string`, `table`, `math`, `utf8` and `bit32` libraries (no I/O,
network, or `require`), a 64 MB memory cap, and a time budget per call (250 ms
for setup, renders and handlers, 60 ms per canvas or scene frame), so a runaway
script fails with an error instead of freezing the app. `math.random` is
reseeded from the visual's id before every call, so random data is identical on
every render and device. The style rules are the API rather than advice: colors
are theme tokens (`text`, `muted`, `faint`, `accent`, `on-accent`, `success`,
`warning`, `danger`, `border`, `chart1`…`chart6`), text sizes are steps on the
transcript's type scale, the font is always the interface font, and layout nodes
have no fill, border, shadow, padding, or font props; using one is an error that
names the rule. 3D scenes render on the GPU (`zeron-visual-gpu`: physically
based materials, shadows, glass with dispersion, glTF models, textures,
environment maps, sprites and bloom), with the runtime's software rasterizer
(`raster.rs`) as the fallback where there's no GPU: Zeron frames an orbit
camera on the geometry and draws it into one transparent image. A visual's
files (`assets`: at most 16, 8 MB each, 10 MB together) are stored beside its
script as `{id}-asset-{name}` with an `{id}-assets.json` list, and travel to
other devices like attachments. `ui.shader` takes Shadertoy-style GLSL
(`mainImage`, `iResolution`, `iTime`, `iMouse`, plus `iCamera` for an orbit
camera, named float inputs bound to the script's state, `iNoise`, and up to four
Shadertoy-style buffer passes read as `iChannel0`-`3`, with feedback from their
own previous frame); `zeron_visual::shader` wraps each pass in a GLSL 4.50
program and validates it with naga, so a compile error reaches the agent with
its pass and line. A shader composites onto the chat by `blend` (`alpha`,
`light` for emitted light where black is clear, or `opaque`) with optional
bloom, so it never sits in a black box.

The engine runs every script before showing it (`zeron_visual::check`: setup,
the first render, every canvas and scene draw, then each tab, toggle, slider
end, and button once) and returns the first error with its line, so the agent
fixes it before the reader sees anything. A passing script is stored in uploads
(`{id}-visual.luau`) and a `visual` doc part is folded into the chat's live turn
through the run's engine event channel; with no live turn on this device the
call fails and nothing is stored. The doc carries only the path and title, so
clients read the script with `ReadAttachmentChunk` like any attachment; old
clients see an empty part.

The desktop (`crates/ui/src/visuals.rs`) renders the tree with the transcript's
own GPUI elements: markdown's table treatment, the settings tab, switch, and
button idioms, canvases as GPUI paths and shaped text in theme colors, and
scenes and shaders as images. Shaders run offscreen on the GPU through wgpu
(`crates/visual-gpu`, one shared device, pipelines cached by source) and are
read back like a scene's image. Nothing foreign is hosted, so a visual scrolls, clips, blurs under
the glass composer, and takes input like any other row, identically on Linux,
macOS, and Windows, and it follows theme, appearance, and text-size changes
without rerunning the script.

### Image replies

`show_image {path}` is listed under the same condition: it shows an image file
(a screenshot, a rendered PNG) inline in the calling chat as the same `image`
doc part generated replies use, so every client already renders it
(the desktop's image row with its lightbox, iOS's image row). The MCP server
reads the file from the agent's working directory with the agent's own access
(at most 10 MB) and sends it base64 in `PublishImage {chatId, name, data}`. The
engine refuses anything but a PNG, JPEG, WebP, or GIF the desktop can preview
(at most 4096 pixels on a side, read from the header), stores it in uploads as
`{id}-image.{ext}`, and folds the part into the live turn through the run's
engine event channel, past the generated-image intake. The doc carries the stored path and the file's name, never the
original path, and the image stays viewable after the source file changes or
is deleted. An agent may also answer "show me" with `![alt](/abs/path.png)`;
the transcript draws such an image in place, from the chat's own folder only and
never copied (see [image-preview.md](image-preview.md#images-in-agent-replies)).
Nothing tells an agent about that: the tool description and server instructions name only
`show_image`, and no harness prompt changes.

## Selecting a host and project

`list_projects {device?}` filters by device id or exact name; without arguments
it lists all projects. `list_harnesses {device?}` and
`list_models {harness, device?}` query the selected host with `targetDeviceId`;
without `device` they retain the local engine catalogs.

For creation, project alone determines the host. Device alone creates a session
without a project on that device. With both, the project is resolved **within**
the selected device, and a project id belonging to another device is rejected.
Neither argument means a projectless session on the local engine. Repeated names
or paths are errors with candidate ids and devices; use ids from discovery.

Before writing, creation checks the chosen engine's harness catalog, chooses
its connected default (DeepSeek when available), and validates any explicit
model against that host's catalog.
An explicit harness must be offered, installed and enabled. Catalog failures,
including model lookup failures, are returned with the device id; there is no
fallback to the local catalog. A successful remote query checks reachability;
`lastSeenAt` alone does not establish that an engine is online.

Discover and launch a standalone session:

```text
list_devices {}
list_projects { device: "<device-id>" }
list_harnesses { device: "<device-id>" }
list_models { device: "<device-id>", harness: "codex" }
create_chat {
  kind: "chat",
  device: "<device-id>",
  project: "<project-id>",
  harness: "deepseek",
  model: "<model-id from that host's catalog>",
  title: "Implement feature",
  prompt: "Implement the feature...",
  wait: false
}
```

Without a project (the host uses its home directory unless `cwd` is given):

```json
{"kind":"chat", "device":"<device-id>", "harness":"deepseek", "prompt":"Reply with pong", "wait":true}
```

A mixed `create_chats` batch, with an origin chat or explicit top-level parent:

```json
{
  "requests": [
    {"kind":"chat", "device":"<device-id>", "project":"<project-id>", "title":"Feature", "prompt":"Implement the feature"},
    {"kind":"side", "parent":"<parent-chat-id>", "project":"<project-id>", "prompt":"Review the API"}
  ]
}
```

The first prompt and later sends use durable chat commands drained by the host.
Transcript reads target the chat's host; session status comes from the shared
registry. Sends remember the session and existing message ids on the MCP connection before
sending, so a subsequent `wait_for_turn` after `wait:false` waits for that send,
even before a session row appears. A separate MCP server has no send baseline
and reports the chat's current posture. Creation records the send time before dispatching its first
prompt. If completion arrives before the transcript, the wait allows the new
assistant response to arrive within the **same timeout**. A previous response
is never substituted for a missing response to a new send. An unfinished
transcript or missing response at the deadline returns `timedOut`.

## Delegating within DeepSeek

The `zeron` server is injected into every DeepSeek run, and `create_chat` takes
a `harness` for compatibility; it must name DeepSeek. `list_harnesses` returns
only DeepSeek (plus Mock under a test/development opt-in):

- `available` — installed and enabled on this device.
- `signedIn` — whether a DeepSeek key is present on the selected host
  (`ListAgentAccounts`), or `null` when that could not be determined.
- `connected` — available and not known to be missing a key. This is what a
  delegating agent should choose from; `create_chat` without a `harness` also
  prefers DeepSeek.

`signedIn` is advisory: credential detection does not see every source (a key
in the environment, say), so only an explicit `false` counts against it.
`create_chat` still creates the chat but returns a `warning` when DeepSeek
looks unconfigured, rather than refusing.

## Historical harness ids

Legacy `HarnessId` values decode so existing rows and transcripts keep their
labels and marks. They are read-only: `create_chat` and `Mutate setChatConfig`
refuse them with an explicit unsupported error, and no run, switch, or resume
is attempted.

## Parallel side chats

Use `create_chats` with a `requests` array to launch independent workers in
one tool call, including when the harness executes its tool calls sequentially:

```json
{
  "requests": [
    {"project": "/repo", "title": "Review tests", "prompt": "Review test coverage"},
    {"project": "/repo", "title": "Review API", "prompt": "Review API compatibility"}
  ]
}
```

Use `send_messages` with the same envelope for existing chats; each item uses
`send_message` arguments. Both accept 1–32 requests, run them concurrently,
and return `results` in input order with `index`, `isError`, and either `result`
or `error`. A failed request does not cancel or roll back successful requests.
Each request defaults to `wait: false`; explicit waits also run concurrently.
Only batch independent work, not ordered messages to the same chat.

When using individual tools, launch **all** chats/messages with `wait: false`
first, then collect replies with `wait_for_turn`. Waiting on each individual
launch before issuing the next serializes work at the caller, even though
both the MCP transport and engine support concurrent chat runs.

## Smoke recipe

```sh
BIN=target/debug/zeron
{ echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_chats","arguments":{"limit":5}}}'
  sleep 5; } | ZERON_IPC_PORT=27655 ZERON_DATA_DIR=/path/to/that/engine/data $BIN mcp
```

`create_chat` with `"prompt": "Reply with exactly the word pong", "wait": true`
against a live daemon returns the assistant's `pong` in a few seconds; archive
the chat afterwards with `archive_chat`. Unit tests (`cargo test -p zeron-mcp`)
drive the whole tool set against an in-memory stub `RpcService`.

An isolated stdio instance is available with
`cargo run -p zeron-engine --example mcp_standalone_smoke`. It provides a temporary
project, an origin coordinator and a scripted `codex` adapter (`smoke-1`) returning
`pong`. Discover its ids with the list tools, create `kind: "chat"` with a prompt
and `wait: true`, then check `read_chat`, `send_message` and a mixed batch. The
profile is removed on exit. This checks MCP dispatch and engine execution without
changing your running app's sessions or requiring a DeepSeek key.

The `mcp_standalone_session_executes_on_the_selected_device` test in
`device_routing` additionally runs two engines with a shared registry and device
relay, verifies execution on the selected host (and absence of execution on the
caller), and retrieves responses through MCP.
