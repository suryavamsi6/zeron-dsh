//! What a plugin declares in its manifest's `settings`, and what the person
//! set for it (the plugins contract's "Settings").
//!
//! A manifest lists them under `settings`, each `{ id, kind, label,
//! description?, default?, section? }` and, by kind:
//!
//! - `switch`: on or off (`default` a boolean, else off);
//! - `choice`: one of `options` (`[{ value, label, description? }]`, values
//!   text), drawn as a `style` of `"select"` (the default) or `"segmented"`;
//! - `text`: words (`placeholder?`), at most [`MAX_TEXT`] characters;
//! - `number`: `min?`, `max?`, `step?`, `unit?`;
//! - `shortcut`: a key combination as Zeron writes them (`ctrl+alt+space`),
//!   or none (`""`);
//! - `folder`: a folder (its absolute path), or none (`""`);
//! - `permission` (Zeron's own plugins alone): a platform permission row,
//!   `{ kind, permission }`. It has no value; the app says everything on it;
//! - `wallpaper`: the row that uses the plugin's drawing as a new chat's
//!   background, for a plugin replacing `#new-chat-background`. It has no
//!   value of the plugin's: the app keeps which plugin draws it.
//!
//! What was set is kept in `<plugins dir>/.storage/<id>.settings.json`, and
//! only what was set: a setting never touched follows the manifest's default,
//! so a default the plugin changes reaches whoever never set it. A value that
//! no longer fits its setting (an option dropped, a kind changed) is ignored
//! for its default. Removing the plugin removes the file
//! ([`forget`]; the engine's `RemovePlugin`).
//!
//! This is the one store and the one set of rules for those values: the
//! engine owns the folder, the file name and the checks, and an app draws the
//! controls and writes through here. It serves no RPC of its own — the file
//! is the plugins folder's, beside the engine's other plugin state.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value as Json, json};

/// The most settings a plugin declares (the rest are left out).
pub const MAX_SETTINGS: usize = 40;
/// The most options a choice has.
pub const MAX_OPTIONS: usize = 24;
/// The most characters a text setting holds.
pub const MAX_TEXT: usize = 2000;
const MAX_LABEL: usize = 80;
const MAX_DESCRIPTION: usize = 400;
/// The most characters a page's own description says.
pub const MAX_PAGE_DESCRIPTION: usize = 200;
/// The setting kinds a manifest may declare.
pub const KINDS: &[&str] = &[
    "switch",
    "choice",
    "text",
    "number",
    "shortcut",
    "folder",
    "permission",
    "wallpaper",
];
/// The place a plugin replaces to draw a new chat's background.
pub const WALLPAPER_PLACE: &str = "#new-chat-background";
/// The platform permissions a `permission` row can be.
pub const PERMISSIONS: &[&str] = &["screen-recording", "accessibility"];
/// The folder under a plugins directory where each plugin's storage is kept.
pub const STORAGE_DIR: &str = ".storage";

/// Whether a row of `kind` is the app's own words, with no value of the
/// plugin's (a permission's, the wallpaper's).
pub fn is_app_row(kind: &str) -> bool {
    kind == "permission" || kind == "wallpaper"
}

/// One declared setting, checked and filled in: what a page draws.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub id: String,
    pub kind: String,
    pub default: Json,
    /// Everything the page reads (label, description, options, …).
    pub json: Json,
}

impl Setting {
    /// Its label, or its id where it says none.
    pub fn label(&self) -> &str {
        self.json
            .get("label")
            .and_then(|l| l.as_str())
            .unwrap_or(&self.id)
    }

    /// The section it stands under, where it says one.
    pub fn section(&self) -> Option<&str> {
        self.json.get("section").and_then(|s| s.as_str())
    }

    /// A choice's options, each `{ value, label, description? }`.
    pub fn options(&self) -> &[Json] {
        self.json
            .get("options")
            .and_then(|o| o.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// A choice drawn as a row of segments rather than a dropdown.
    pub fn segmented(&self) -> bool {
        self.json.get("style").and_then(|s| s.as_str()) == Some("segmented")
    }
}

/// What a manifest says under its page's title (`settings_description`),
/// plain, or none.
pub fn page_description(manifest: &Json) -> Option<String> {
    let text = plain(
        manifest.get("settings_description")?.as_str()?,
        MAX_PAGE_DESCRIPTION,
    );
    (!text.is_empty()).then_some(text)
}

fn short(value: Option<&Json>, most: usize) -> Option<String> {
    let s = value?.as_str()?.trim();
    (!s.is_empty()).then(|| s.chars().take(most).collect())
}

fn number(value: Option<&Json>) -> Option<f64> {
    value?.as_f64().filter(|n| n.is_finite())
}

/// One line of plain text, at most `most` characters: control characters
/// (new lines among them) become spaces.
fn plain(text: &str, most: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let words: Vec<&str> = flat.split_whitespace().collect();
    let cut: String = words.join(" ").chars().take(most).collect();
    cut.trim_end().to_string()
}

/// A setting's id: letters, digits, `-`, `_` and `.`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A key combination as Zeron writes them: modifiers then a key, `+`
/// between, at least one of Control, Option or Command held.
pub fn valid_shortcut(combo: &str) -> bool {
    let parts: Vec<&str> = combo.split('+').collect();
    let Some((key, mods)) = parts.split_last() else {
        return false;
    };
    let known = ["ctrl", "alt", "shift", "cmd"];
    let held = mods.iter().any(|m| matches!(*m, "ctrl" | "alt" | "cmd"));
    let key_ok = !key.is_empty()
        && key.len() <= 10
        && key.chars().all(|c| c.is_ascii_alphanumeric())
        && !known.contains(key);
    held && key_ok && mods.iter().all(|m| known.contains(m)) && combo.len() <= 40
}

/// `value` as `setting` holds it, or none where it doesn't fit.
pub fn fit(setting: &Setting, value: &Json) -> Option<Json> {
    let j = &setting.json;
    match setting.kind.as_str() {
        "switch" => value.as_bool().map(Json::Bool),
        "choice" => {
            let v = value.as_str()?;
            setting
                .options()
                .iter()
                .any(|o| o.get("value").and_then(|v| v.as_str()) == Some(v))
                .then(|| json!(v))
        }
        "text" => value
            .as_str()
            .map(|s| json!(s.chars().take(MAX_TEXT).collect::<String>())),
        "number" => {
            let mut n = number(Some(value))?;
            if let Some(min) = number(j.get("min")) {
                n = n.max(min);
            }
            if let Some(max) = number(j.get("max")) {
                n = n.min(max);
            }
            Some(json!(n))
        }
        "shortcut" => value
            .as_str()
            .filter(|s| s.is_empty() || valid_shortcut(s))
            .map(|s| json!(s)),
        "folder" => value
            .as_str()
            .filter(|s| s.is_empty() || (Path::new(s).is_absolute() && s.len() <= 4096))
            .map(|s| json!(s)),
        _ => None,
    }
}

/// The settings `manifest` declares, each checked: one that isn't a setting
/// (no id, an unknown kind, a choice with no options, an id used already, a
/// permission row of a plugin not Zeron's own: `builtin`) is left out.
pub fn declared(manifest: &Json, builtin: bool) -> Vec<Setting> {
    let mut out: Vec<Setting> = Vec::new();
    let Some(list) = manifest.get("settings").and_then(|s| s.as_array()) else {
        return out;
    };
    for raw in list.iter().take(MAX_SETTINGS) {
        // A permission row's id is its permission, unless it says another;
        // a wallpaper row's is always `wallpaper`.
        let id = raw.get("id").and_then(|i| i.as_str()).or(
            match raw.get("kind").and_then(|k| k.as_str()) {
                Some("permission") => raw.get("permission").and_then(|p| p.as_str()),
                Some("wallpaper") => Some("wallpaper"),
                _ => None,
            },
        );
        let Some(id) = id.filter(|id| valid_id(id)) else {
            continue;
        };
        if out.iter().any(|s| s.id == id) {
            continue;
        }
        let Some(kind) = raw.get("kind").and_then(|k| k.as_str()) else {
            continue;
        };
        if !KINDS.contains(&kind) {
            continue;
        }
        if kind == "wallpaper" {
            let replaces = manifest
                .get("ui")
                .and_then(|ui| ui.get("change"))
                .and_then(|c| c.as_array())
                .is_some_and(|list| list.iter().any(|s| s.as_str() == Some(WALLPAPER_PLACE)));
            if !replaces || out.iter().any(|s| s.kind == "wallpaper") {
                continue;
            }
            // Nothing of the plugin's on it: Zeron says it all.
            let mut json = json!({ "id": id, "kind": kind });
            if let Some(section) = short(raw.get("section"), MAX_LABEL) {
                json["section"] = json!(section);
            }
            out.push(Setting {
                id: id.to_string(),
                kind: kind.to_string(),
                default: Json::Null,
                json,
            });
            continue;
        }
        if kind == "permission" {
            if !builtin {
                continue;
            }
            let Some(permission) = raw
                .get("permission")
                .and_then(|p| p.as_str())
                .filter(|p| PERMISSIONS.contains(p))
            else {
                continue;
            };
            let mut json = json!({ "id": id, "kind": kind, "permission": permission });
            if let Some(section) = short(raw.get("section"), MAX_LABEL) {
                json["section"] = json!(section);
            }
            out.push(Setting {
                id: id.to_string(),
                kind: kind.to_string(),
                default: Json::Null,
                json,
            });
            continue;
        }
        let label = short(raw.get("label"), MAX_LABEL).unwrap_or_else(|| id.to_string());
        let mut json = json!({ "id": id, "kind": kind, "label": label });
        if let Some(description) = short(raw.get("description"), MAX_DESCRIPTION) {
            json["description"] = json!(description);
        }
        if let Some(section) = short(raw.get("section"), MAX_LABEL) {
            json["section"] = json!(section);
        }
        match kind {
            "choice" => {
                let options: Vec<Json> = raw
                    .get("options")
                    .and_then(|o| o.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|o| {
                        let value = o
                            .get("value")
                            .and_then(|v| v.as_str())
                            .filter(|v| !v.is_empty() && v.len() <= 200)?;
                        let label =
                            short(o.get("label"), MAX_LABEL).unwrap_or_else(|| value.to_string());
                        let mut option = json!({ "value": value, "label": label });
                        if let Some(description) = short(o.get("description"), MAX_DESCRIPTION) {
                            option["description"] = json!(description);
                        }
                        Some(option)
                    })
                    .take(MAX_OPTIONS)
                    .collect();
                if options.is_empty() {
                    continue;
                }
                json["options"] = Json::Array(options);
                json["style"] = json!(if raw.get("style").and_then(|s| s.as_str())
                    == Some("segmented")
                {
                    "segmented"
                } else {
                    "select"
                });
            }
            "text" => {
                if let Some(placeholder) = short(raw.get("placeholder"), MAX_LABEL) {
                    json["placeholder"] = json!(placeholder);
                }
            }
            "number" => {
                for key in ["min", "max", "step"] {
                    if let Some(n) = number(raw.get(key)) {
                        json[key] = json!(n);
                    }
                }
                if let (Some(min), Some(max)) = (number(json.get("min")), number(json.get("max")))
                    && min > max
                {
                    continue;
                }
                if let Some(unit) = short(raw.get("unit"), 24) {
                    json["unit"] = json!(unit);
                }
            }
            _ => {}
        }
        let mut setting = Setting {
            id: id.to_string(),
            kind: kind.to_string(),
            default: Json::Null,
            json,
        };
        // Its default: the manifest's where it fits, else the kind's own
        // (off, the first option, nothing, the least it takes).
        let fallback = match kind {
            "switch" => json!(false),
            "choice" => setting.options()[0]
                .get("value")
                .cloned()
                .unwrap_or(Json::Null),
            "number" => fit(&setting, &json!(0.0)).unwrap_or_else(|| json!(0.0)),
            _ => json!(""),
        };
        let asked = raw.get("default");
        let default = asked.and_then(|d| fit(&setting, d));
        if asked.is_some_and(|d| !d.is_null()) && default.is_none() {
            setting.json["ignored_default"] = json!(true);
        }
        setting.default = default.unwrap_or(fallback);
        setting.json["default"] = setting.default.clone();
        out.push(setting);
    }
    out
}

/// [`declared`]'s values: what was set where it still fits, else its default
/// (a permission or wallpaper row has none).
pub fn values(declared: &[Setting], stored: &Map<String, Json>) -> Map<String, Json> {
    declared
        .iter()
        .filter(|s| !is_app_row(&s.kind))
        .map(|s| {
            (
                s.id.clone(),
                stored
                    .get(&s.id)
                    .and_then(|v| fit(s, v))
                    .unwrap_or_else(|| s.default.clone()),
            )
        })
        .collect()
}

/// One plugin's declared settings and the values read from its file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginSettings {
    /// The plugin's id (the manifest's).
    pub id: String,
    /// What the manifest declares, in the order it declares them.
    pub declared: Vec<Setting>,
    /// What the person set, kept as written (only values that still fit).
    pub stored: Map<String, Json>,
    /// The page's own line under its title.
    pub description: Option<String>,
    /// Its settings file there but unreadable: [`PluginSettings::set`]
    /// refuses, so a save can't drop what was kept.
    load_error: Option<String>,
}

impl PluginSettings {
    /// Read `manifest`'s declared settings and the values `storage` keeps
    /// for the plugin it names.
    pub fn read(storage: &Path, manifest: &Json, builtin: bool) -> Self {
        let id = manifest
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or_default()
            .to_string();
        let declared = declared(manifest, builtin);
        let (stored, load_error) = if declared.iter().any(|s| !is_app_row(&s.kind)) {
            match load(storage, &id) {
                Ok(stored) => (stored, None),
                Err(error) => (Map::new(), Some(error)),
            }
        } else {
            (Map::new(), None)
        };
        Self {
            id,
            declared,
            stored,
            description: page_description(manifest),
            load_error,
        }
    }

    /// Every setting with its value now: what was set where it fits, else
    /// its default.
    pub fn values(&self) -> Map<String, Json> {
        values(&self.declared, &self.stored)
    }

    /// One setting's value now.
    pub fn value(&self, id: &str) -> Option<Json> {
        self.values().get(id).cloned()
    }

    /// Whether it declares settings a page draws at all (rows of the app's
    /// own alone: no page of its).
    pub fn has_page(&self) -> bool {
        self.declared.iter().any(|s| !is_app_row(&s.kind))
    }

    /// Set `id` to `value` (`null`: back to its default) in `storage`,
    /// returning whether the file changed. A value that doesn't fit its
    /// setting is refused with what it takes instead, and so is any change
    /// while its settings file can't be read back — a save would drop what
    /// it keeps.
    pub fn set(&mut self, storage: &Path, id: &str, value: &Json) -> Result<bool, String> {
        if let Some(error) = self.load_error.as_ref() {
            return Err(format!("{error}, so it can't be changed"));
        }
        let Some(setting) = self
            .declared
            .iter()
            .find(|s| s.id == id && !is_app_row(&s.kind))
        else {
            return Err(format!("\"{id}\" isn't one of this plugin's settings"));
        };
        let held = if value.is_null() {
            None
        } else {
            Some(fit(setting, value).ok_or_else(|| {
                format!(
                    "\"{value}\" doesn't fit {} ({})",
                    setting.label(),
                    setting.kind
                )
            })?)
        };
        let mut next = self.stored.clone();
        match held {
            Some(v) => {
                next.insert(id.to_string(), v);
            }
            None => {
                next.remove(id);
            }
        }
        if next == self.stored {
            return Ok(false);
        }
        store(storage, &self.id, &next)?;
        self.stored = next;
        Ok(true)
    }
}

/// Where plugin `id`'s settings are kept, under a `.storage` folder.
pub fn path_in(storage: &Path, id: &str) -> Option<PathBuf> {
    let safe = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    safe.then(|| storage.join(format!("{id}.settings.json")))
}

/// The `.storage` folder inside a plugins directory.
pub fn storage_in(plugins_dir: &Path) -> PathBuf {
    plugins_dir.join(STORAGE_DIR)
}

/// Whether `storage` is a link: the store never follows one to a folder of
/// the link's choosing.
fn storage_is_link(storage: &Path) -> bool {
    std::fs::symlink_metadata(storage)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// What `id` keeps in `storage`: an object, or none. A file there but
/// unreadable is an error — never nothing kept, which a later save would
/// write over.
pub fn load(storage: &Path, id: &str) -> Result<Map<String, Json>, String> {
    if storage_is_link(storage) {
        return Err("its settings folder is a link".to_string());
    }
    let Some(path) = path_in(storage, id) else {
        return Ok(Map::new());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(format!("its settings file can't be read ({e})")),
    };
    let value = serde_json::from_str::<Json>(&text)
        .map_err(|e| format!("its settings file can't be read ({e})"))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "its settings file isn't a JSON object".to_string())
}

/// Keep `held` as `id`'s settings: nothing kept means no file. Written
/// beside the file then renamed over it, the temporary file made, never
/// followed — a link there fails instead of writing where it points.
pub fn store(storage: &Path, id: &str, held: &Map<String, Json>) -> Result<(), String> {
    let path = path_in(storage, id).ok_or("there's nowhere to keep it")?;
    if storage_is_link(storage) {
        return Err("its settings folder is a link".to_string());
    }
    if held.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    std::fs::create_dir_all(storage).map_err(|e| e.to_string())?;
    let text = serde_json::to_string(held).map_err(|e| e.to_string())?;
    let aside = path.with_extension("json.new");
    let write_aside = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&aside)
    };
    let mut file = write_aside().or_else(|e| {
        // One left over from an interrupted save may go; a link or anything
        // else may not be touched.
        if e.kind() == std::io::ErrorKind::AlreadyExists
            && std::fs::symlink_metadata(&aside).is_ok_and(|m| m.is_file())
        {
            std::fs::remove_file(&aside).map_err(|e| e.to_string())?;
        }
        write_aside().map_err(|e| e.to_string())
    })?;
    std::io::Write::write_all(&mut file, text.as_bytes()).map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&aside, &path).map_err(|e| e.to_string())
}

/// Plugin `id`'s settings let go of (it was removed).
pub fn forget(storage: &Path, id: &str) {
    if storage_is_link(storage) {
        return;
    }
    if let Some(path) = path_in(storage, id) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> Json {
        json!({
            "id": "demo",
            "settings": [
                { "id": "on", "kind": "switch", "label": "On", "default": true },
                { "id": "tone", "kind": "choice", "label": "Tone", "style": "segmented", "default": "warm",
                  "options": [ { "value": "warm", "label": "Warm" }, { "value": "dry", "label": "Dry" } ] },
                { "id": "greeting", "kind": "text", "label": "Greeting", "default": "Hello", "placeholder": "Hi there" },
                { "id": "every", "kind": "number", "label": "Every", "min": 30, "max": 600, "unit": "seconds", "default": 60 },
                { "id": "keys", "kind": "shortcut", "label": "Shortcut", "default": "ctrl+alt+g" },
                { "id": "out", "kind": "folder", "label": "Save to" },
                { "id": "bad", "kind": "colour", "label": "Nope" },
                { "id": "empty", "kind": "choice", "label": "No options" },
                { "id": "on", "kind": "switch", "label": "Twice" },
            ],
        })
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zeron-plugin-settings-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_manifest_declares_its_settings_each_checked() {
        let d = declared(&manifest(), false);
        let ids: Vec<&str> = d.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["on", "tone", "greeting", "every", "keys", "out"],
            "unknown kinds, empty choices and repeats are left out"
        );
        assert!(d[1].segmented());
        assert_eq!(d[1].json["style"], "segmented");
        assert_eq!(d[3].json["unit"], "seconds");
        assert_eq!(d[5].default, json!(""), "a folder starts with none");
        assert_eq!(d[2].json["placeholder"], "Hi there");
        // A default that doesn't fit falls back to the kind's own.
        let m = json!({ "settings": [
            { "id": "n", "kind": "number", "min": 5, "default": "six" },
            { "id": "c", "kind": "choice", "options": [{ "value": "a" }], "default": "z" },
        ] });
        let d = declared(&m, false);
        assert_eq!(
            (d[0].default.clone(), d[1].default.clone()),
            (json!(5.0), json!("a"))
        );
        assert_eq!(
            d[1].json["options"][0]["label"], "a",
            "an option's label defaults to its value"
        );
        assert_eq!(
            d[0].json["ignored_default"], true,
            "the app can say the default was dropped"
        );
    }

    #[test]
    fn values_fit_their_setting_or_fall_back() {
        let d = declared(&manifest(), false);
        let tone = &d[1];
        assert_eq!(fit(tone, &json!("dry")), Some(json!("dry")));
        assert_eq!(fit(tone, &json!("loud")), None);
        assert_eq!(
            fit(&d[3], &json!(9000)),
            Some(json!(600.0)),
            "a number is kept within its bounds"
        );
        assert_eq!(
            fit(&d[4], &json!("cmd+shift+k")),
            Some(json!("cmd+shift+k"))
        );
        assert_eq!(
            fit(&d[4], &json!("shift+k")),
            None,
            "a shortcut holds Control, Option or Command"
        );
        assert_eq!(fit(&d[4], &json!("")), Some(json!("")), "or is none");
        assert_eq!(fit(&d[5], &json!("relative/path")), None);
        let pictures = if cfg!(windows) {
            r"C:\Users\me\Pictures"
        } else {
            "/Users/me/Pictures"
        };
        assert_eq!(fit(&d[5], &json!(pictures)), Some(json!(pictures)));
        assert_eq!(fit(&d[0], &json!("yes")), None);
        assert_eq!(
            fit(&d[2], &json!("a".repeat(MAX_TEXT + 10))),
            Some(json!("a".repeat(MAX_TEXT)))
        );
    }

    #[test]
    fn what_you_set_survives_a_restart_and_only_that_is_kept() {
        let dir = scratch("restart");
        let mut settings = PluginSettings::read(&dir, &manifest(), false);
        assert_eq!(settings.values()["tone"], "warm");
        assert!(settings.set(&dir, "tone", &json!("dry")).unwrap());
        assert!(
            !settings.set(&dir, "tone", &json!("dry")).unwrap(),
            "the same again changes nothing"
        );
        assert!(settings.set(&dir, "tone", &json!("loud")).is_err());
        let kept: Json =
            serde_json::from_str(&std::fs::read_to_string(dir.join("demo.settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            kept,
            json!({ "tone": "dry" }),
            "defaults aren't written down"
        );
        // A new run.
        let mut again = PluginSettings::read(&dir, &manifest(), false);
        assert_eq!(again.values()["tone"], "dry");
        // Back to its default: nothing kept, the file gone.
        assert!(again.set(&dir, "tone", &Json::Null).unwrap());
        assert_eq!(again.values()["tone"], "warm");
        assert!(!dir.join("demo.settings.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_changed_default_reaches_whoever_never_set_it() {
        let dir = scratch("defaults");
        let mut settings = PluginSettings::read(&dir, &manifest(), false);
        settings.set(&dir, "tone", &json!("dry")).unwrap();
        let mut m = manifest();
        m["settings"][2]["default"] = json!("Howdy");
        m["settings"][1]["default"] = json!("warm");
        let again = PluginSettings::read(&dir, &m, false);
        let v = again.values();
        assert_eq!(
            (v["greeting"].as_str(), v["tone"].as_str()),
            (Some("Howdy"), Some("dry")),
            "the new default, what you set kept"
        );
        // An option dropped: what you'd chosen goes back to the default.
        m["settings"][1]["options"] = json!([{ "value": "warm" }]);
        let third = PluginSettings::read(&dir, &m, false);
        assert_eq!(third.values()["tone"], "warm");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn each_plugins_are_its_own_and_go_when_it_is_removed() {
        let dir = scratch("own");
        let mut settings = PluginSettings::read(&dir, &manifest(), false);
        settings.set(&dir, "greeting", &json!("Mine")).unwrap();
        let mut other = manifest();
        other["id"] = json!("another");
        let second = PluginSettings::read(&dir, &other, false);
        assert_eq!(second.values()["greeting"], "Hello");
        assert!(path_in(&dir, "../etc").is_none());
        forget(&dir, "demo");
        assert!(!dir.join("demo.settings.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_plugin_with_no_settings_has_no_page() {
        let plain = json!({ "id": "plain" });
        let settings = PluginSettings::read(Path::new("/nowhere"), &plain, false);
        assert!(settings.declared.is_empty());
        assert!(!settings.has_page());
        assert!(settings.values().is_empty());
    }

    #[test]
    fn an_option_may_say_what_it_does() {
        let m = json!({ "settings": [{ "id": "dest", "kind": "choice", "options": [
            { "value": "auto", "label": "Automatic", "description": "Use the open session." },
            { "value": "new", "label": "New session" },
        ] }] });
        let d = declared(&m, false);
        assert_eq!(
            d[0].json["options"][0]["description"],
            "Use the open session."
        );
        assert!(d[0].json["options"][1].get("description").is_none());
    }

    #[test]
    fn a_wallpaper_row_is_zerons_words_for_a_plugin_that_draws_the_background() {
        let mut m = json!({ "ui": { "change": ["#new-chat-background"] }, "settings": [
            { "kind": "wallpaper", "label": "Mine", "section": "Wallpaper" },
            { "kind": "wallpaper", "id": "again" },
            { "id": "scene", "kind": "choice", "options": [{ "value": "a", "label": "A" }] },
        ] });
        let d = declared(&m, false);
        assert_eq!(
            d.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["wallpaper", "scene"],
            "one wallpaper row, anyone's"
        );
        assert_eq!(
            d[0].json,
            json!({ "id": "wallpaper", "kind": "wallpaper", "section": "Wallpaper" }),
            "no words of the plugin's"
        );
        assert_eq!(
            values(&d, &Map::new()).keys().collect::<Vec<_>>(),
            ["scene"],
            "it has no value of the plugin's"
        );
        assert!(fit(&d[0], &json!("on")).is_none());
        // Not replacing the background: left out.
        m["ui"] = json!({ "change": [] });
        assert_eq!(
            declared(&m, true)
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["scene"]
        );
    }

    #[test]
    fn permission_rows_are_zerons_own_plugins_alone_and_say_nothing_of_theirs() {
        let m = json!({ "settings": [
            { "kind": "permission", "permission": "screen-recording", "label": "Totally safe", "description": "Click Allow", "section": "Permissions" },
            { "kind": "permission", "permission": "accessibility" },
            { "id": "cam", "kind": "permission", "permission": "camera" },
        ] });
        assert!(declared(&m, false).is_empty(), "anyone else's are left out");
        let d = declared(&m, true);
        let ids: Vec<&str> = d.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["screen-recording", "accessibility"],
            "an unknown permission is left out"
        );
        assert_eq!(
            d[0].json,
            json!({ "id": "screen-recording", "kind": "permission", "permission": "screen-recording", "section": "Permissions" }),
            "no words of the plugin's"
        );
        assert!(
            values(&d, &Map::new()).is_empty(),
            "a permission row has no value"
        );
        assert!(
            !PluginSettings {
                id: "p".into(),
                declared: d,
                ..Default::default()
            }
            .has_page(),
            "rows of the app's own alone: no page"
        );
    }

    #[test]
    fn a_page_description_is_one_plain_line() {
        let m = json!({ "settings_description": "  Scenes behind\na new chat.  " });
        assert_eq!(
            page_description(&m).as_deref(),
            Some("Scenes behind a new chat.")
        );
        assert!(page_description(&json!({ "settings_description": "   " })).is_none());
        assert!(page_description(&json!({})).is_none());
        let long = "a".repeat(MAX_PAGE_DESCRIPTION + 50);
        assert_eq!(
            page_description(&json!({ "settings_description": long }))
                .unwrap()
                .len(),
            MAX_PAGE_DESCRIPTION
        );
    }

    #[test]
    fn a_shortcut_is_modifiers_then_a_key() {
        assert!(valid_shortcut("ctrl+alt+space"));
        assert!(valid_shortcut("cmd+k"));
        assert!(!valid_shortcut("k"));
        assert!(!valid_shortcut("shift+k"), "Shift alone is no modifier");
        assert!(!valid_shortcut("ctrl+"));
        assert!(!valid_shortcut("ctrl+alt"));
        assert!(!valid_shortcut("super+k"));
        assert!(!valid_shortcut("ctrl+k+1"));
    }

    #[test]
    fn a_settings_file_is_an_object_and_a_plugin_id_is_a_file_name() {
        let dir = scratch("file");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            load(&dir, "demo").unwrap().is_empty(),
            "none there: nothing kept"
        );
        std::fs::write(dir.join("demo.settings.json"), "{\"tone\":\"dry\"}").unwrap();
        assert_eq!(load(&dir, "demo").unwrap()["tone"], "dry");
        assert_eq!(
            storage_in(Path::new("/d/plugins")),
            PathBuf::from("/d/plugins/.storage")
        );
        assert!(path_in(&dir, "a/b").is_none());
        assert!(path_in(&dir, "").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_there_but_unreadable_isnt_emptied_or_written_over() {
        let dir = scratch("unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("demo.settings.json"), "[1,2]").unwrap();
        assert!(load(&dir, "demo").is_err(), "not an object: unreadable");
        std::fs::write(dir.join("demo.settings.json"), "{ not json").unwrap();
        let mut settings = PluginSettings::read(&dir, &manifest(), false);
        assert!(
            settings.set(&dir, "tone", &json!("dry")).is_err(),
            "a save would drop what it keeps, so it's refused"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("demo.settings.json")).unwrap(),
            "{ not json",
            "the file it couldn't read stays as it was"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_that_fails_keeps_what_it_knew_and_can_be_tried_again() {
        let dir = scratch("retry");
        std::fs::create_dir_all(&dir).unwrap();
        // The temporary file as a folder: the write fails.
        std::fs::create_dir_all(dir.join("demo.settings.json.new")).unwrap();
        let mut settings = PluginSettings::read(&dir, &manifest(), false);
        assert!(settings.set(&dir, "tone", &json!("dry")).is_err());
        assert_eq!(settings.stored, Map::new(), "nothing kept was made up");
        std::fs::remove_dir(dir.join("demo.settings.json.new")).unwrap();
        assert!(
            settings.set(&dir, "tone", &json!("dry")).unwrap(),
            "the same value asked again tries the save again"
        );
        assert_eq!(settings.stored["tone"], "dry");
        // Nothing kept means no file — and removing it failing is a failure.
        std::fs::remove_file(dir.join("demo.settings.json")).unwrap();
        std::fs::create_dir(dir.join("demo.settings.json")).unwrap();
        assert!(
            store(&dir, "demo", &Map::new()).is_err(),
            "what couldn't be removed isn't said gone"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_isnt_followed_to_where_it_points() {
        let dir = scratch("links");
        let inside = dir.join("storage");
        let outside = dir.join("elsewhere");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // `<id>.settings.json.new` a link: its target isn't written.
        let victim = outside.join("victim.txt");
        std::fs::write(&victim, "precious").unwrap();
        std::os::unix::fs::symlink(&victim, inside.join("demo.settings.json.new")).unwrap();
        let mut settings = PluginSettings::read(&inside, &manifest(), false);
        assert!(settings.set(&inside, "tone", &json!("dry")).is_err());
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "precious",
            "the link's target was never written"
        );
        assert!(!inside.join("demo.settings.json").exists());
        // `.storage` itself a link: read and write are both refused.
        let linked = dir.join("linked");
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        let settings = PluginSettings::read(&linked, &manifest(), false);
        assert_eq!(settings.values()["tone"], "warm");
        let mut held = Map::new();
        held.insert("tone".to_string(), json!("dry"));
        assert!(
            store(&linked, "demo", &held).is_err()
                && store(&linked, "demo", &Map::new()).is_err(),
            "a linked folder keeps nothing and takes nothing"
        );
        assert!(!outside.join("demo.settings.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
