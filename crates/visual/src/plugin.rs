//! A plugin's drawing, run in a visual's sandbox: its entry, the modules it
//! requires, and the places it replaces.
//!
//! A plugin is a Luau program that says where it draws (`zeron.place`) and
//! hands back a node tree for that place when the app asks (`ui.div`,
//! `ui.image`, and the rest of the tree in [`crate::Node`]). It reads the
//! settings its manifest declares (`zeron.setting`), where the pointer is
//! over the window (`zeron.live("pointer")`), and its own words
//! (`zeron.s`). Nothing of the app is reachable: no I/O, no clock, no
//! window, and the same memory and time limits a visual has.
//!
//! What the host answers is a deliberate subset of the shipping app's
//! plugin API: `zeron.place`, `setting`/`settings`, `live`, `s`, `context`,
//! `need`, and the `ui` tree pieces a drawing needs (`div`, `image`,
//! `computed`, `spring`, `presence`). Anything else a plugin reaches for
//! raises an error naming what is missing rather than silently drawing
//! nothing.
//!
//! A build runs once per frame while the drawing is on screen: the tree is
//! rebuilt from the plugin's own view function, with `ui.spring` advancing
//! by the time since the last build, so a drawing moves as the pointer does.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{Function, Lua, LuaOptions, StdLib, Table, Value, VmState};
use serde_json::{Map, Value as Json};

use crate::{Node, Parser, VisualError};

/// The largest plugin entry this host runs.
pub const MAX_ENTRY_BYTES: usize = 512 * 1024;
/// A module a plugin requires may be this large.
pub const MAX_MODULE_BYTES: usize = 512 * 1024;
const MEMORY_LIMIT: usize = 128 * 1024 * 1024;
/// Loading a plugin (its entry and every module it requires).
const LOAD_BUDGET: Duration = Duration::from_millis(2_000);
/// One build of a place's tree. A drawing runs this per frame, so it has to
/// be quick: a plugin that spends longer is told why.
const BUILD_BUDGET: Duration = Duration::from_millis(16);
const MAX_LOG_LINES: usize = 200;
/// One printed line is capped, so a long argument can't grow a log without
/// limit.
const MAX_LOG_LINE_BYTES: usize = 4 * 1024;
/// What all the kept lines may hold together: each argument is formatted
/// into a Rust `String` outside the VM's memory limit, so the copies need
/// their own budget.
const MAX_LOG_BYTES: usize = 256 * 1024;

/// A plugin's own modules, by the name a `require` asks for: the path in
/// its folder without `./` or `.luau` (`scenes`, `rule/colour`). The app
/// reads the folder; the sandbox runs the sources.
#[derive(Debug, Default, Clone)]
pub struct Modules {
    files: HashMap<String, String>,
}

impl Modules {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `name`'s source, where `name` is the path without `./` or the
    /// extension.
    pub fn insert(&mut self, name: impl Into<String>, source: impl Into<String>) {
        self.files.insert(name.into(), source.into());
    }

    /// `name`'s source: the longest module whose name is a suffix of it
    /// (`rule/colour` matches `require("./rule/colour")`, and a bare
    /// `scenes` matches `require("./scenes")`).
    pub fn source(&self, name: &str) -> Option<&str> {
        let name = name.trim_start_matches("./");
        self.files.get(name).map(String::as_str)
    }
}

/// How a plugin puts its node at a place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    Replace,
    Before,
    After,
}

impl How {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

/// One place a plugin draws at.
pub struct PlaceEntry {
    /// The selector it named (`#new-chat-background`).
    pub at: String,
    pub how: How,
    /// It takes the whole room the element had, not just what it needs.
    pub fill: bool,
    build: Function,
}

/// A build's frame: where the pointer is, how long since the last build,
/// and each spring's state.
#[derive(Default)]
struct Tick {
    pointer: Option<(f32, f32)>,
    /// Seconds since the last build, at most [`Tick::MAX_STEP`].
    dt: f32,
    /// Which spring the build is on (springs are keyed by the order they
    /// are created in a build, which is stable while the tree's shape is).
    spring: usize,
    springs: Vec<Spring>,
}

impl Tick {
    /// A frame that waited this long never animates a jump: it settles.
    const MAX_STEP: f32 = 0.1;
}

#[derive(Debug, Default, Clone, Copy)]
struct Spring {
    value: f32,
    velocity: f32,
}

impl Spring {
    /// Follow `target` for `dt` seconds with `stiffness` and `damping`
    /// (semi-implicit Euler, in steps small enough to stay steady).
    fn advance(&mut self, target: f32, dt: f32, stiffness: f32, damping: f32) -> f32 {
        let mut left = dt.clamp(0.0, Tick::MAX_STEP);
        while left > 0.0 {
            let step = left.min(1.0 / 240.0);
            let force = -stiffness * (self.value - target) - damping * self.velocity;
            self.velocity += force * step;
            self.value += self.velocity * step;
            left -= step;
        }
        if !self.value.is_finite() {
            // (A stiffness or damping that made the maths diverge: land.)
            *self = Self {
                value: target,
                velocity: 0.0,
            };
        }
        self.value
    }
}

/// A plugin's live program.
pub struct PluginScript {
    lua: Lua,
    places: Vec<PlaceEntry>,
    tick: Rc<RefCell<Tick>>,
    last: Rc<Cell<Option<Instant>>>,
    deadline: Rc<Cell<Option<Instant>>>,
    logs: Rc<RefCell<Vec<String>>>,
    /// The modules it required, by the name it asked for.
    loaded: Rc<RefCell<HashMap<String, String>>>,
}

impl PluginScript {
    /// Load `entry` with the modules it requires, its settings and its words.
    pub fn load(
        entry: &str,
        modules: &Modules,
        settings: &Map<String, Json>,
        locale: &std::collections::BTreeMap<String, String>,
        reduced_motion: bool,
    ) -> Result<Self, VisualError> {
        if entry.len() > MAX_ENTRY_BYTES {
            return Err(VisualError::script(format!(
                "the plugin's entry is over {} KB",
                MAX_ENTRY_BYTES / 1024
            )));
        }
        let lua = Lua::new_with(
            StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::BIT,
            LuaOptions::default(),
        )?;
        lua.set_memory_limit(MEMORY_LIMIT)?;

        let deadline: Rc<Cell<Option<Instant>>> = Rc::default();
        let ticks = Cell::new(0u32);
        let watched = deadline.clone();
        lua.set_interrupt(move |_| {
            let tick = ticks.get().wrapping_add(1);
            ticks.set(tick);
            if tick.is_multiple_of(256)
                && let Some(deadline) = watched.get()
                && Instant::now() > deadline
            {
                return Err(mlua::Error::RuntimeError(
                    "the plugin ran past its time budget (an endless loop, or too much work \
                     per frame)"
                        .into(),
                ));
            }
            Ok(VmState::Continue)
        });

        let logs: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = logs.clone();
        let globals = lua.globals();
        globals.set(
            "print",
            lua.create_function(move |_, args: mlua::Variadic<Value>| {
                let mut logs = sink.borrow_mut();
                if logs.len() < MAX_LOG_LINES {
                    let kept: usize = logs.iter().map(String::len).sum();
                    let left = MAX_LOG_BYTES.saturating_sub(kept);
                    if left > 0 {
                        let mut line = String::new();
                        for (at, arg) in args.iter().enumerate() {
                            let cap = MAX_LOG_LINE_BYTES.min(left);
                            // A separator counts toward the cap, so the
                            // assembled line is never pushed over it.
                            let room = cap.saturating_sub(line.len() + usize::from(at > 0));
                            if room == 0 {
                                break;
                            }
                            if at > 0 {
                                line.push('\t');
                            }
                            let text = match arg {
                                // A long string is cut from its bytes first,
                                // so the copy out of the VM is bounded too.
                                Value::String(text) => {
                                    let bytes = text.as_bytes();
                                    String::from_utf8_lossy(&bytes[..room.min(bytes.len())])
                                        .into_owned()
                                }
                                other => crate::node::value_text(other),
                            };
                            // The copy can run long where a cut sits inside a
                            // UTF-8 sequence; bound what the line gets.
                            let mut end = room.min(text.len());
                            while !text.is_char_boundary(end) {
                                end -= 1;
                            }
                            line.push_str(&text[..end]);
                        }
                        logs.push(line);
                    }
                }
                Ok(())
            })?,
        )?;

        let places: Rc<RefCell<Vec<(String, How, bool, Function)>>> = Rc::default();
        let tick: Rc<RefCell<Tick>> = Rc::default();
        let loaded: Rc<RefCell<HashMap<String, String>>> = Rc::default();

        globals.set("ui", ui_table(&lua, &tick)?)?;
        let zeron = zeron_table(&lua, settings, locale, &tick, &places)?;
        globals.set("zeron", zeron.clone())?;
        let require = require_function(
            &lua,
            modules,
            &zeron,
            locale,
            reduced_motion,
            &loaded,
        )?;
        globals.set("require", require)?;
        lua.sandbox(true)?;

        deadline.set(Some(Instant::now() + LOAD_BUDGET));
        let result = lua
            .load(entry)
            .set_name("=plugin")
            .set_mode(mlua::chunk::ChunkMode::Text)
            .exec();
        deadline.set(None);
        result?;

        let places = places
            .borrow_mut()
            .drain(..)
            .map(|(at, how, fill, build)| PlaceEntry {
                at,
                how,
                fill,
                build,
            })
            .collect();
        Ok(Self {
            lua,
            places,
            tick,
            last: Rc::default(),
            deadline,
            logs,
            loaded,
        })
    }

    /// The places it draws at, in the order it declared them.
    pub fn places(&self) -> &[PlaceEntry] {
        &self.places
    }

    /// Whether it replaces `at`, as a new chat's background must
    /// (`#new-chat-background`).
    pub fn replaces(&self, at: &str) -> bool {
        self.places
            .iter()
            .any(|place| place.at == at && place.how == How::Replace)
    }

    /// Build `at`'s node now, with the pointer over the window (`0..1`, both
    /// axes) and the time since the last build.
    pub fn build(
        &mut self,
        at: &str,
        pointer: Option<(f32, f32)>,
        now: Instant,
    ) -> Result<Option<Node>, VisualError> {
        let Some(index) = self
            .places
            .iter()
            .position(|place| place.at == at && place.how == How::Replace)
        else {
            return Ok(None);
        };
        let dt = self
            .last
            .replace(Some(now))
            .map_or(0.0, |last| now.saturating_duration_since(last).as_secs_f32());
        {
            let mut tick = self.tick.borrow_mut();
            tick.pointer = pointer;
            tick.dt = dt;
            tick.spring = 0;
        }
        let build = self.places[index].build.clone();
        let fill = self.places[index].fill;
        self.deadline.set(Some(now + BUILD_BUDGET));
        // The budget covers the parse too: reading the tree can run Lua
        // (`__index`, `__len`), and a metamethod that loops has to stop.
        let value = build.call::<Value>(()).map_err(VisualError::from);
        let node = value.and_then(|value| Parser::new().root(value));
        self.deadline.set(None);
        let node = node?;
        let node = if fill {
            Node {
                kind: crate::NodeKind::Frame(Box::new(crate::Frame {
                    width_full: true,
                    height_full: true,
                    children: vec![node],
                    ..Default::default()
                })),
                width: None,
                grow: true,
                key: None,
            }
        } else {
            node
        };
        Ok(Some(node))
    }

    /// Whether a spring moved in the last build: the drawing needs another
    /// frame to settle.
    pub fn animating(&self) -> bool {
        let tick = self.tick.borrow();
        tick.springs
            .iter()
            .take(tick.spring)
            .any(|spring| spring.velocity.abs() > 1.0 || {
                // A spring whose target moved this frame is on its way.
                spring.velocity != 0.0
            })
    }

    /// Lines the plugin printed, oldest first.
    pub fn logs(&self) -> Vec<String> {
        self.logs.borrow().clone()
    }

    /// The modules it required, by the name it asked for (what a report of
    /// what it reached for says).
    pub fn required(&self) -> Vec<String> {
        let mut names: Vec<String> = self.loaded.borrow().keys().cloned().collect();
        names.sort();
        names
    }
}

/// The `ui` table a plugin draws with.
fn ui_table(lua: &Lua, tick: &Rc<RefCell<Tick>>) -> Result<Table, VisualError> {
    let ui = lua.create_table()?;
    // Constructors only tag their props table; the host parses the tree.
    let constructor = lua.create_function(|lua, (kind, props): (String, Value)| {
        let table = match props {
            Value::Table(props) => props,
            other => {
                let props = lua.create_table()?;
                props.set(1, other)?;
                props
            }
        };
        table.set("__kind", kind)?;
        Ok(table)
    })?;
    for kind in ["div", "image"] {
        let make = constructor.clone();
        ui.set(
            kind,
            lua.create_function(move |_, props: Value| make.call::<Table>((kind, props)))?,
        )?;
    }
    // A computed value is asked for at once: a build runs every frame, so
    // there is nothing to follow.
    ui.set(
        "computed",
        lua.create_function(|_, function: Function| function.call::<Value>(()))?,
    )?;
    // A keyed swap: the keyed build's node, with the props it was given.
    ui.set(
        "presence",
        lua.create_function(|lua, (key, render, props): (Function, Function, Option<Table>)| {
            let key = key.call::<Value>(())?;
            let node = render.call::<Value>(key)?;
            if let (Value::Table(node), Some(props)) = (&node, props) {
                for pair in props.pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    node.set(k, v)?;
                }
            }
            let _ = lua;
            Ok(node)
        })?,
    )?;
    // A spring: `target` (0..1, or any number) followed toward `to` from
    // `from`, by the time since the last build.
    let state = tick.clone();
    ui.set(
        "spring",
        lua.create_function(move |_, (target, options): (f64, Option<Table>)| {
            let mut stiffness = 140.0f32;
            let mut damping = 20.0f32;
            let (mut from, mut to) = (0.0f32, 1.0f32);
            if let Some(options) = options {
                if let Some(value) = options.get::<Option<f64>>("stiffness")? {
                    stiffness = value.clamp(1.0, 2000.0) as f32;
                }
                if let Some(value) = options.get::<Option<f64>>("damping")? {
                    damping = value.clamp(0.0, 500.0) as f32;
                }
                if let Some(value) = options.get::<Option<f64>>("from")? {
                    from = value.clamp(-8192.0, 8192.0) as f32;
                }
                if let Some(value) = options.get::<Option<f64>>("to")? {
                    to = value.clamp(-8192.0, 8192.0) as f32;
                }
            }
            let target = from + (to - from) * target.clamp(-8.0, 8.0) as f32;
            let mut state = state.borrow_mut();
            let index = state.spring;
            state.spring += 1;
            if state.springs.len() <= index {
                state.springs.push(Spring {
                    value: target,
                    velocity: 0.0,
                });
            }
            let dt = state.dt;
            let spring = &mut state.springs[index];
            Ok(f64::from(spring.advance(target, dt, stiffness, damping)))
        })?,
    )?;
    Ok(ui)
}

/// The `zeron` table: what a plugin reads and where it draws.
fn zeron_table(
    lua: &Lua,
    settings: &Map<String, Json>,
    locale: &std::collections::BTreeMap<String, String>,
    tick: &Rc<RefCell<Tick>>,
    places: &Rc<RefCell<Vec<(String, How, bool, Function)>>>,
) -> Result<Table, VisualError> {
    let zeron = lua.create_table()?;
    let held = lua.create_table()?;
    for (key, value) in settings {
        held.set(key.as_str(), json_to_lua(lua, value)?)?;
    }
    zeron.set("settings", held.clone())?;
    zeron.set(
        "setting",
        lua.create_function(move |lua, id: String| {
            let value = held.get::<Option<Value>>(id.as_str())?.unwrap_or(Value::Nil);
            let get = lua.create_function(move |_, ()| Ok(value.clone()))?;
            let setting = lua.create_table()?;
            setting.set("get", get.clone())?;
            setting.set("peek", get)?;
            Ok(setting)
        })?,
    )?;
    // Where the pointer is over the window, 0 to 1 (0.5 at rest).
    let state = tick.clone();
    zeron.set(
        "live",
        lua.create_function(move |lua, name: String| {
            let state = state.clone();
            let function = lua.create_function(move |lua, ()| {
                let pointer = state.borrow().pointer.unwrap_or((0.5, 0.5));
                let record = lua.create_table()?;
                record.set("x", f64::from(pointer.0))?;
                record.set("y", f64::from(pointer.1))?;
                Ok(record)
            })?;
            let _ = &name;
            Ok(function)
        })?,
    )?;
    // The place it draws at: `{ at, before?, after?, replace?, fill? }`.
    let declared = places.clone();
    zeron.set(
        "place",
        lua.create_function(move |_, spec: Table| {
            let at: String = spec.get("at")?;
            let (how, build) = if let Some(build) = spec.get::<Option<Function>>("replace")? {
                (How::Replace, build)
            } else if let Some(build) = spec.get::<Option<Function>>("before")? {
                (How::Before, build)
            } else if let Some(build) = spec.get::<Option<Function>>("after")? {
                (How::After, build)
            } else {
                return Err(mlua::Error::RuntimeError(
                    "zeron.place needs before, after or replace".into(),
                ));
            };
            let fill = spec.get::<Option<bool>>("fill")?.unwrap_or(false);
            let mut declared = declared.borrow_mut();
            if declared
                .iter()
                .any(|(there, how, _, _)| *there == at && *how == How::Replace)
            {
                return Err(mlua::Error::RuntimeError(format!(
                    "zeron.place: \"{at}\" is already replaced"
                )));
            }
            declared.push((at, how, fill, build));
            Ok(())
        })?,
    )?;
    // Its own words, by dotted key (`scene.violet_dusk`).
    let words = locale.clone();
    zeron.set(
        "s",
        lua.create_function(move |_, (key, args): (String, Option<Table>)| {
            let Some(template) = words.get(&key) else {
                return Ok(key);
            };
            let mut out = String::new();
            let mut rest = template.as_str();
            while let Some(start) = rest.find('{') {
                let Some(end) = rest[start..].find('}') else {
                    break;
                };
                out.push_str(&rest[..start]);
                let name = &rest[start + 1..start + end];
                match args.as_ref().and_then(|args| args.get::<Option<Value>>(name).ok().flatten()) {
                    Some(Value::String(text)) => out.push_str(&text.to_string_lossy()),
                    Some(Value::Integer(number)) => out.push_str(&number.to_string()),
                    Some(Value::Number(number)) => out.push_str(&number.to_string()),
                    _ => out.push_str(&format!("{{{name}}}")),
                }
                rest = &rest[start + end + 1..];
            }
            out.push_str(rest);
            Ok(out)
        })?,
    )?;
    // What a place gives the node built in it: nothing yet in this host (a
    // new chat's background gives the pointer, read with `live`).
    zeron.set(
        "context",
        lua.create_function(|_, _: String| Ok(Value::Nil))?,
    )?;
    zeron.set(
        "need",
        lua.create_function(|_, name: String| -> mlua::Result<Value> {
            Err(mlua::Error::RuntimeError(format!(
                "this host gives no \"{name}\" to a node yet"
            )))
        })?,
    )?;
    // Everything else says what it is, so a plugin can be written against
    // the rest of the API and told where this build stops.
    let missing = lua.create_table()?;
    missing.set(
        "__index",
        lua.create_function(|_, name: String| -> mlua::Result<Value> {
            Err(mlua::Error::RuntimeError(format!(
                "the GPUI plugin host doesn't serve zeron.{name} yet"
            )))
        })?,
    )?;
    zeron.set_metatable(Some(missing))?;
    Ok(zeron)
}

/// `require`: the plugin's own modules from its folder, Zeron's helpers for
/// the API this host serves, and the design-system pieces a drawing asks
/// for.
fn require_function(
    lua: &Lua,
    modules: &Modules,
    zeron: &Table,
    locale: &std::collections::BTreeMap<String, String>,
    reduced_motion: bool,
    loaded: &Rc<RefCell<HashMap<String, String>>>,
) -> Result<Function, VisualError> {
    let modules = modules.clone();
    let zeron = lua.create_registry_value(zeron.clone())?;
    let locale = locale.clone();
    let loaded = loaded.clone();
    // The modules already run, by name: a module runs once, as Lua's
    // `require` does.
    let cache: Rc<RefCell<HashMap<String, mlua::RegistryKey>>> = Rc::default();
    let registry = Rc::new(lua.create_registry_value(lua.create_table()?)?);
    Ok(lua.create_function(move |lua, name: String| -> mlua::Result<Value> {
        let bare = name.trim_start_matches("./");
        loaded.borrow_mut().insert(name.clone(), bare.to_string());
        // Zeron's own helpers, as the plugin's `zeron.luau` would give them:
        // one function taking the plugin's id and answering the API table.
        if bare == "zeron" || name == "@zeron" {
            let zeron: Table = lua.registry_value(&zeron)?;
            let helpers = lua.create_function(move |_, _id: Option<String>| Ok(zeron.clone()))?;
            return Ok(Value::Function(helpers));
        }
        // The design system: what a drawing asks of it here.
        if name == "@ds/motion" {
            let motion = lua.create_table()?;
            let reduce = lua.create_function(move |_, ()| Ok(reduced_motion))?;
            motion.set("reduced", reduce.clone())?;
            motion.set("reduced_now", reduce)?;
            motion.set(
                "ms",
                lua.create_function(move |_, ms: f64| {
                    Ok(if reduced_motion { 0.0 } else { ms })
                })?,
            )?;
            motion.set("FADE", 120.0f64)?;
            return Ok(Value::Table(motion));
        }
        if let Some(key) = cache.borrow().get(bare) {
            return lua.registry_value::<Value>(key);
        }
        let Some(source) = modules.source(&bare) else {
            let _ = &locale;
            return Err(mlua::Error::RuntimeError(format!(
                "the plugin requires \"{name}\", which isn't in its folder"
            )));
        };
        if source.len() > MAX_MODULE_BYTES {
            return Err(mlua::Error::RuntimeError(format!(
                "\"{name}\" is over {} KB",
                MAX_MODULE_BYTES / 1024
            )));
        }
        let value = lua
            .load(source)
            .set_name(name.as_str())
            .set_mode(mlua::chunk::ChunkMode::Text)
            .eval::<Value>()?;
        let key = lua.create_registry_value(value.clone())?;
        cache.borrow_mut().insert(bare.to_string(), key);
        let _ = &registry;
        Ok(value)
    })?)
}

/// A JSON value as a Lua one.
fn json_to_lua(lua: &Lua, value: &Json) -> Result<Value, VisualError> {
    Ok(match value {
        Json::Null => Value::Nil,
        Json::Bool(flag) => Value::Boolean(*flag),
        Json::Number(number) => Value::Number(number.as_f64().unwrap_or_default()),
        Json::String(text) => Value::String(lua.create_string(text)?),
        Json::Array(items) => {
            let table = lua.create_table()?;
            for (index, item) in items.iter().enumerate() {
                table.set(index + 1, json_to_lua(lua, item)?)?;
            }
            Value::Table(table)
        }
        Json::Object(map) => {
            let table = lua.create_table()?;
            for (key, item) in map {
                table.set(key.as_str(), json_to_lua(lua, item)?)?;
            }
            Value::Table(table)
        }
    })
}
