# Plugin host

A plugin is a folder the engine reports in `ListPlugins`: a
`zeron-plugin.json` manifest, an entry (`init.luau`), whatever `.luau`
modules it requires, `locales/`, and its art. It is not HTML, CSS, or
JavaScript: a plugin draws by handing the app a tree of nodes, and the app
draws that tree with its own elements.

`crates/visual/src/plugin.rs` runs it. `crates/ui/src/plugins.rs` draws what
it hands back; `crates/ui/src/settings/plugins.rs` draws its settings.

## What runs

A plugin's entry and its modules run in one `mlua` VM with a subset of the
standard library (`table`, `string`, `math`, `utf8`, `bit`), 128 MB of
memory, and a sandbox. Its budgets:

| | |
| --- | --- |
| Loading (entry and every module it requires) | 2 s |
| One build of a place's tree | 16 ms |
| The entry, and any module | 512 KB |
| Lines it may `print` | 200 |

`require` answers three things:

- `./zeron` — the host's API table, as a function taking the plugin's id
  (so `require("./zeron")("my-plugin")` reads the same as it does against the
  shipping `zeron.luau`).
- `@ds/motion` — `reduced()`, `reduced_now()`, `ms(n)`, `FADE`.
- Its own files, by path without `./` or `.luau`: `require("./scenes")`.

## What it draws with

Two node constructors, and the props each accepts:

- `ui.div { absolute, left, right, top, bottom, x, y, overflow, relative,
  w_full, h_full, role, label, gap, align, justify, id, class, ref, ... }`
- `ui.image { src, fit, w_full, h_full, aria_hidden, label }` — a picture
  from the plugin's folder, `fit` one of `cover`, `contain`, `fill`.

And the three state helpers a drawing needs while it moves:

- `ui.computed(f)` — asked for at once, each build.
- `ui.presence(key, render, props)` — the node `render(key())` builds.
- `ui.spring(target, { from, to, stiffness, damping })` — a number that
  follows `target` by the frame's time step. Rest is the middle (0.5).

`zeron` carries `place`, `setting`, `settings`, `live`, `s`, `context`, and
`need`. Every other name on it raises an error naming what is missing
(`the GPUI plugin host doesn't serve zeron.service yet`) rather than
drawing nothing, and a `ui.<name>` that isn't there is `nil`, so calling it
says which field it wanted. Props are checked the same way: an unknown prop
lists the ones the node knows.

## What the app does with it

- **Places.** `#new-chat-background` is the one place the app serves today,
  and only as a `replace`. A plugin is offered as the wallpaper while it is
  on and its `ui.change` names that place; a plugin drawing elsewhere is
  listed in Settings but draws nothing.
- **Settings.** Its values come from
  `<data>/plugins/.storage/<id>.settings.json`, fitted to what the manifest
  declares, with the manifest's `default` where the person set nothing. A
  write to that file reloads the plugin.
- **The pointer.** `zeron.live("pointer")` is where the pointer is over the
  page, `0` to `1` on each axis, and `0.5` at rest.
- **Motion.** `crate::motion::reduced_motion` reaches the plugin through
  `@ds/motion`, so a plugin can hold still when the person asks it to.
- **Frames.** A scene whose springs are moving asks for the next frame;
  one at rest is built once and stops. Nothing blocks the main thread.
- **Failure.** A plugin that won't load, or won't draw, leaves the app's
  own image wallpaper in place. Nothing is invented to cover for it.

## What it does not serve yet

The host runs a drawing, not an application. Measured against the shipping
built-ins in `apps/zgx/plugins-builtin`, by the names each one's own files
reach for:

| Plugin | `ui.*` it needs | Not served | `zeron.*` it needs | Not served |
| --- | --- | --- | --- | --- |
| `parallax-scenes` | 5 | 0 | 4 | 0 |
| `git` | 25 | 22 | 32 | 27 |
| `github` | 25 | 22 | 25 | 20 |

Parallax scenes, the new-chat wallpaper, is the one that fits. `git` and
`github` need, in the order that would unlock them:

1. **Stateful components** — `ui.use_state`, `ui.state`, `ui.effect`,
   `ui.component`, `ui.after`, `ui.on_cleanup`, `ui.every`, `ui.task`,
   `ui.invoke`, `ui.call`, `ui.call_sync`, `ui.shared`, `ui.ref`,
   `ui.use_context`.
2. **Layout and chrome** — `ui.row`, `ui.column`, `ui.text`, `ui.show`,
   `ui.list`, `ui.scroll`, `ui.popover`, `ui.canvas`, `ui.raw`, `ui.slot`,
   `ui.override`, `ui.contribute`, `ui.command`, `ui.switch`, `ui.persist`,
   `ui.clipboard`, `ui.path`.
3. **Values, time, theme** — `ui.json`, `ui.now`, `ui.tween`, `ui.theme`,
   `ui.filter`, `ui.add`, `ui.null`.
4. **The engine's own work** — `zeron.service`, `zeron.run`, `zeron.watch`,
   `zeron.store`, `zeron.sessions`, `zeron.panel`, `zeron.tab`, `zeron.menu`,
   `zeron.menu_entries`, `zeron.window`, `zeron.diff`, `zeron.folders`,
   `zeron.device`, `zeron.space`, `zeron.slot`, `zeron.serve`, `zeron.emit`,
   `zeron.sleep`, `zeron.list`, `zeron.page`, `zeron.open_url`,
   `zeron.notify`, `zeron.toast`, `zeron.context_of`, `zeron.secondary`,
   `zeron.composed`, `zeron.compose_comment`, `zeron.reason`.
5. **Its words and the app's tone** — `zeron.t`, `zeron.locale`, `zeron.tone`,
   `zeron.visual`.
6. **GitHub's data** — `zeron.github`, `zeron.pr`, `zeron.checks`,
   `zeron.projects`, `zeron.db`, `zeron.open`.

`ui.canvas` and `ui.raw` are the two that would also need decisions the app
has made so far: a plugin painting pixels, and a plugin bypassing the node
tree, are both surfaces no drawing here has yet.

Until those exist, the app lists both plugins and flips them, and they draw
nothing — which is visible and honest, where a WebView or a stand-in would
not be.
