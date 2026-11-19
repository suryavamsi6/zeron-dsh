//! `DshPluginIndex`: a plugin index's entries as this device lists them —
//! the index fetched once (a small bounded GET), each entry's declared
//! versions checked, the best one recommended for an install by
//! `name@version`.
//!
//! The index is a `format: 1` JSON document: a title (a string, or a map
//! by locale) and its plugin entries, each carrying `versions` in any
//! order. The version rule is deliberately the semver crate's alone: a
//! version is compatible when its `engines.dsh` is absent — or doesn't
//! parse, which reads as absent but is noted — or parses and matches the
//! version through [`semver::VersionReq`]. The recommendation is the
//! highest compatible version that isn't a prerelease, else the highest
//! compatible prerelease; an entry with none is listed as incompatible.
//! Only an npm-sourced version installs through `DshPluginAdd` (the index
//! names the package): a git one stays listed, its reason in place of an
//! install.
//!
//! Answered on this device's own IPC only (`zeron_rpc::local_only`): the
//! fetch goes out from this device to an address the caller names.

use std::collections::BTreeMap;
use std::time::Duration;

use futures::StreamExt as _;
use serde::Deserialize;
use serde_json::{Value, json};
use zeron_rpc::RpcError;

use crate::http_error::describe_http_error;

/// An index's body may not pass 2 MB: a declared length over it refuses
/// the fetch before a byte moves, and a chunked body (which declares
/// none) is refused as the bytes arrive.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// A small JSON document answers fast or not at all.
const INDEX_TIMEOUT: Duration = Duration::from_secs(30);
/// What the index is called when it doesn't say (`title` is optional).
const NO_TITLE: &str = "Plugin index";

/// Why an index couldn't be listed; the message is written for the person.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct IndexError(String);

/// The index document: `format` and fields the model doesn't name are
/// ignored, as are entries' unknown fields.
#[derive(Debug, Deserialize)]
pub struct PluginIndex {
    #[serde(default)]
    title: Option<LocalizedText>,
    #[serde(default)]
    plugins: Vec<IndexPlugin>,
}

/// One plugin's entry in the index.
#[derive(Debug, Deserialize)]
struct IndexPlugin {
    name: String,
    #[serde(default)]
    title: Option<LocalizedText>,
    #[serde(default)]
    description: Option<LocalizedText>,
    /// A string, or an object carrying a `name`; anything else is none.
    #[serde(default)]
    author: Option<Value>,
    /// Required by the format and at least one; an entry that breaks it
    /// still lists, as incompatible.
    #[serde(default)]
    versions: Vec<IndexVersion>,
}

/// One version an entry offers. `version`, `source` and `engines` are
/// read loosely — the wrong JSON type on one is the same as its absence,
/// not a reason to drop the index.
#[derive(Debug, Deserialize)]
struct IndexVersion {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    source: Option<Value>,
    #[serde(default)]
    engines: Option<Value>,
}

/// A text in one language, or a map of them by locale (`{"en": "…"}`):
/// the index's `title` fields take either shape.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LocalizedText {
    Text(String),
    Map(BTreeMap<String, String>),
}

impl LocalizedText {
    /// The text in `locale`: its own entry, then its base language's
    /// (`en` for `en-US`), then English, then any the map has; `fallback`
    /// when it has none.
    fn resolve(&self, locale: &str, fallback: &str) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Map(map) => map
                .get(locale)
                .or_else(|| locale.split(['-', '_']).next().and_then(|lang| map.get(lang)))
                .or_else(|| map.get("en"))
                .or_else(|| map.values().next())
                .cloned()
                .unwrap_or_else(|| fallback.to_owned()),
        }
    }
}

/// `{url, locale?}` → `{title, plugins}`: the index fetched and reported.
/// `call` for `DshPluginIndex` is this — a network read that takes no
/// install lock.
pub async fn index(url: &str, locale: &str) -> Result<Value, IndexError> {
    Ok(fetch(&client(), url).await?.report(locale))
}

/// The index at `url`, fetched and parsed: http(s) only, credentials and
/// any fragment stripped, the body refused over [`MAX_BYTES`] as it
/// arrives.
async fn fetch(client: &reqwest::Client, url: &str) -> Result<PluginIndex, IndexError> {
    let url = sanitized(url)?;
    let response = client.get(url).send().await.map_err(reach)?;
    let status = response.status();
    if !status.is_success() {
        return Err(IndexError(format!(
            "the index answered HTTP {}",
            status.as_u16()
        )));
    }
    fits(response.content_length())?;
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        push(&mut body, &chunk.map_err(reach)?)?;
    }
    parse(&body)
}

/// The URL the request leaves with: http(s) alone, without credentials
/// or a fragment — neither is the index's, and neither should be sent or
/// logged with the fetch.
fn sanitized(url: &str) -> Result<reqwest::Url, IndexError> {
    let mut url = reqwest::Url::parse(url.trim())
        .map_err(|_| IndexError("that isn't an http(s) URL an index can be fetched from".into()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(IndexError(
            "only http and https index URLs can be fetched".into(),
        ));
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    Ok(url)
}

/// Whether a body of `declared` bytes fits under the cap.
fn fits(declared: Option<u64>) -> Result<(), IndexError> {
    if declared.is_some_and(|bytes| bytes > MAX_BYTES) {
        return Err(IndexError(format!(
            "the index is bigger than {} MiB",
            MAX_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

/// `chunk` onto `body`, refusing the moment it grows past the cap.
fn push(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), IndexError> {
    fits(Some((body.len() + chunk.len()) as u64))?;
    body.extend_from_slice(chunk);
    Ok(())
}

/// The body as the index document; anything else is not an index.
fn parse(body: &[u8]) -> Result<PluginIndex, IndexError> {
    serde_json::from_slice(body)
        .map_err(|error| IndexError(format!("that address didn't answer a plugin index ({error})")))
}

fn reach(error: reqwest::Error) -> IndexError {
    IndexError(format!(
        "the index couldn't be fetched: {}",
        describe_http_error(error)
    ))
}

/// The fetch's own client: a bounded wait on a small read.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(INDEX_TIMEOUT)
        .user_agent(concat!("zeron/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// A version's `engines.dsh` text parsed: `Some(Ok)` a range that means
/// something, `Some(Err)` a string the version rule reads as absent —
/// and notes — and `None` genuinely absent.
fn engines_dsh(version: &IndexVersion) -> Option<Result<semver::VersionReq, semver::Error>> {
    let text = version.engines.as_ref()?.get("dsh")?.as_str()?;
    Some(semver::VersionReq::parse(text))
}

/// Whether a version is compatible: it parses as semver (which the
/// caller's candidates already did) and its `engines.dsh`, if it has one
/// that parses, matches it. The deliberately simple rule — npm's own
/// range grammar is out of scope.
fn compatible(version: &semver::Version, entry: &IndexVersion) -> bool {
    match engines_dsh(entry) {
        Some(Ok(req)) => req.matches(version),
        _ => true,
    }
}

/// The source `type` a version names — `"npm"`, `"git"`, or another; an
/// absent `source` reads as npm, the install's own path.
fn source_kind(version: &IndexVersion) -> &str {
    version
        .source
        .as_ref()
        .and_then(|source| source.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("npm")
}

/// An entry's author: a string, or an object's `name`.
fn author(plugin: &IndexPlugin) -> Option<String> {
    match plugin.author.as_ref()? {
        Value::String(name) => Some(name.clone()),
        Value::Object(map) => map.get("name")?.as_str().map(str::to_owned),
        _ => None,
    }
}

impl PluginIndex {
    /// The index's title in `locale` ([`NO_TITLE`] when it has none) and
    /// each entry as the page reports it.
    pub fn report(&self, locale: &str) -> Value {
        let title = self
            .title
            .as_ref()
            .map_or_else(|| NO_TITLE.to_owned(), |title| title.resolve(locale, NO_TITLE));
        json!({
            "title": title,
            "plugins": self.plugins.iter().map(|plugin| plugin.report(locale)).collect::<Vec<Value>>(),
        })
    }
}

impl IndexPlugin {
    /// The entry's report: its name, display title, author, the version
    /// recommended for it with that version's source, whether it can be
    /// installed here (npm yes, anything else no), and why not — or what
    /// couldn't be checked — as a note.
    fn report(&self, locale: &str) -> Value {
        let title = self
            .title
            .as_ref()
            .map_or_else(|| self.name.clone(), |title| title.resolve(locale, &self.name));
        let description = self
            .description
            .as_ref()
            .map(|text| text.resolve(locale, ""));
        let author = author(self);
        let candidates: Vec<(semver::Version, &IndexVersion)> = self
            .versions
            .iter()
            .filter_map(|entry| {
                entry
                    .version
                    .as_deref()
                    .and_then(|text| semver::Version::parse(text.trim()).ok())
                    .map(|version| (version, entry))
            })
            .collect();
        let compatible: Vec<&(semver::Version, &IndexVersion)> = candidates
            .iter()
            .filter(|(version, entry)| compatible(version, entry))
            .collect();
        // The highest compatible non-prerelease; none of those, the
        // highest compatible prerelease; none at all, incompatible.
        let best = compatible
            .iter()
            .filter(|(version, _)| version.pre.is_empty())
            .max_by(|a, b| a.0.cmp(&b.0))
            .or_else(|| compatible.iter().max_by(|a, b| a.0.cmp(&b.0)));
        let Some((version, entry)) = best else {
            return self.base(
                title,
                description,
                author,
                Value::Null,
                Value::Null,
                false,
                Some(if self.versions.is_empty() {
                    "This entry lists no versions.".to_owned()
                } else {
                    "No listed version is compatible with this device.".to_owned()
                }),
            );
        };
        let kind = source_kind(entry);
        let installable = kind == "npm";
        let mut notes = Vec::new();
        if !installable {
            notes.push(if kind == "git" {
                "Installs from a git repository — add it by its URL.".to_owned()
            } else {
                format!("Installs from {kind:?}, which can't be installed here.")
            });
        }
        if let Some(Err(_)) = engines_dsh(entry) {
            notes.push("Its engines.dsh couldn't be read — compatibility is unknown.".to_owned());
        }
        self.base(
            title,
            description,
            author,
            json!(version.to_string()),
            json!(kind),
            installable,
            (!notes.is_empty()).then(|| notes.join(" ")),
        )
    }

    /// The report's fixed shape: every key present on every entry, the
    /// ones nothing is known of null.
    fn base(
        &self,
        title: String,
        description: Option<String>,
        author: Option<String>,
        version: Value,
        source: Value,
        installable: bool,
        note: Option<String>,
    ) -> Value {
        json!({
            "name": self.name,
            "title": title,
            "description": description,
            "author": author,
            "version": version,
            "source": source,
            "installable": installable,
            "note": note,
        })
    }
}

/// `params` for `DshPluginIndex`: `{url}` and an optional `{locale}`.
pub fn params(params: &Value) -> Result<(&str, &str), RpcError> {
    let url = params
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| RpcError::BadParams("index takes a url string".into()))?;
    let locale = params
        .get("locale")
        .and_then(Value::as_str)
        .unwrap_or("en");
    Ok((url, locale))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The index the tests parse: mixed sources, versions out of order,
    /// both title shapes, an incompatible entry, and fields the model
    /// doesn't name.
    const FIXTURE: &str = r##"{
        "format": 1,
        "title": { "en": "Team plugins", "zh-Hans": "团队插件" },
        "plugins": [
            {
                "name": "dsh-plugin-foo",
                "title": { "en": "Foo tools" },
                "description": { "en": "Foo's own tools" },
                "author": "Ada Dev",
                "homepage": "https://index.invalid/foo",
                "repository": { "url": "https://index.invalid/foo.git" },
                "versions": [
                    { "version": "1.1.0", "publishTimestamp": 1790476800, "source": { "type": "npm" }, "engines": { "dsh": ">=0.2.0-rc.2" } },
                    { "version": "1.0.0", "publishTimestamp": 1790000000, "source": { "type": "npm" } },
                    { "version": "2.0.0-rc.1", "source": { "type": "npm" } }
                ]
            },
            {
                "name": "dsh-plugin-bar",
                "author": { "name": "Grace" },
                "versions": [
                    { "version": "0.4.0", "source": { "type": "git", "url": "https://index.invalid/bar.git", "commit": "abc123" } }
                ]
            },
            {
                "name": "dsh-plugin-broken",
                "title": "Broken",
                "versions": [
                    { "version": "1.0.0", "source": { "type": "npm" }, "engines": { "dsh": ">=9.9.9" } }
                ]
            },
            {
                "name": "dsh-plugin-mystery",
                "versions": [
                    { "version": "1.2.3", "source": { "type": "npm" }, "engines": { "dsh": "not a range" } }
                ]
            }
        ],
        "somethingElse": { "nested": true }
    }"##;

    fn plugins(index: &PluginIndex, locale: &str) -> Vec<Value> {
        index.report(locale)["plugins"].as_array().unwrap().clone()
    }

    #[test]
    fn a_valid_mixed_source_index_reports_each_entry() {
        let index = parse(FIXTURE.as_bytes()).unwrap();
        let report = index.report("en");
        assert_eq!(report["title"], "Team plugins");
        let plugins = report["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 4);
        let foo = &plugins[0];
        assert_eq!(foo["name"], "dsh-plugin-foo");
        assert_eq!(foo["title"], "Foo tools");
        assert_eq!(foo["description"], "Foo's own tools");
        assert_eq!(foo["author"], "Ada Dev");
        // 1.1.0 over 2.0.0-rc.1: the highest compatible non-prerelease.
        assert_eq!(foo["version"], "1.1.0");
        assert_eq!(foo["source"], "npm");
        assert_eq!(foo["installable"], true);
        assert_eq!(foo["note"], Value::Null);
        // A named author's `name` is the one reported; no title, the name.
        let bar = &plugins[1];
        assert_eq!(bar["title"], "dsh-plugin-bar");
        assert_eq!(bar["author"], "Grace");
        assert_eq!(bar["description"], Value::Null);
    }

    #[test]
    fn versions_out_of_order_still_pick_the_highest_compatible() {
        for order in [
            vec!["1.0.0", "1.1.0", "1.0.5"],
            vec!["1.1.0", "1.0.5", "1.0.0"],
            vec!["1.0.5", "1.0.0", "1.1.0"],
        ] {
            let body = json!({
                "plugins": [{
                    "name": "p",
                    "versions": order.iter().map(|v| json!({ "version": v })).collect::<Vec<_>>(),
                }]
            });
            let index = parse(body.to_string().as_bytes()).unwrap();
            let plugins = plugins(&index, "en");
            assert_eq!(plugins[0]["version"], "1.1.0", "{order:?}");
        }
    }

    #[test]
    fn a_title_is_a_string_or_a_locale_map() {
        // The map resolves the asked locale, then its base language,
        // then English, then any.
        let index = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(index.report("zh-Hans")["title"], "团队插件");
        assert_eq!(index.report("fr")["title"], "Team plugins");
        let entry = plugins(&index, "zh-Hans").remove(0);
        // An entry title that is only English falls back to it.
        assert_eq!(entry["title"], "Foo tools");
        let plain = parse(br#"{"title": "Team plugins", "plugins": []}"#).unwrap();
        assert_eq!(plain.report("zh-Hans")["title"], "Team plugins");
        // None at all, and an empty map, each take the fallback.
        let none = parse(br#"{"plugins": []}"#).unwrap();
        assert_eq!(none.report("en")["title"], NO_TITLE);
        let empty = parse(br#"{"title": {}, "plugins": []}"#).unwrap();
        assert_eq!(empty.report("en")["title"], NO_TITLE);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // format, publishTimestamp, homepage, repository, somethingElse:
        // present in FIXTURE and all ignored.
        let index = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(plugins(&index, "en").len(), 4);
    }

    #[test]
    fn malformed_json_is_refused() {
        for body in [b"{not json".as_slice(), b"42", b"[]", b""] {
            assert!(parse(body).is_err(), "{body:?}");
        }
    }

    #[test]
    fn an_entry_whose_engines_dsh_matches_nothing_is_incompatible() {
        let plugins = plugins(&parse(FIXTURE.as_bytes()).unwrap(), "en");
        let broken = &plugins[2];
        assert_eq!(broken["name"], "dsh-plugin-broken");
        assert_eq!(broken["version"], Value::Null);
        assert_eq!(broken["source"], Value::Null);
        assert_eq!(broken["installable"], false);
        assert!(
            broken["note"].as_str().unwrap().contains("compatible"),
            "{}",
            broken["note"]
        );
        // An entry listing no versions is incompatible the same way.
        let index = parse(br#"{"plugins": [{"name": "empty"}]}"#).unwrap();
        let plugins = plugins(&index, "en");
        assert_eq!(plugins[0]["installable"], false);
        assert_eq!(plugins[0]["version"], Value::Null);
        assert!(plugins[0]["note"].as_str().unwrap().contains("no versions"));
    }

    #[test]
    fn an_unparsable_engines_dsh_reads_as_absent_noted_unknown() {
        let plugins = plugins(&parse(FIXTURE.as_bytes()).unwrap(), "en");
        let mystery = &plugins[3];
        assert_eq!(mystery["version"], "1.2.3");
        assert_eq!(mystery["installable"], true);
        assert!(
            mystery["note"].as_str().unwrap().contains("unknown"),
            "{}",
            mystery["note"]
        );
    }

    #[test]
    fn a_git_source_is_listed_but_not_installable() {
        let plugins = plugins(&parse(FIXTURE.as_bytes()).unwrap(), "en");
        let bar = &plugins[1];
        assert_eq!(bar["name"], "dsh-plugin-bar");
        assert_eq!(bar["version"], "0.4.0");
        assert_eq!(bar["source"], "git");
        assert_eq!(bar["installable"], false);
        assert!(bar["note"].as_str().unwrap().contains("git"), "{}", bar["note"]);
    }

    #[test]
    fn a_prerelease_is_chosen_only_when_nothing_stable_is() {
        let index = parse(
            br#"{"plugins": [{"name": "rc", "versions": [
                { "version": "2.0.0-rc.1" },
                { "version": "2.0.0-rc.3" },
                { "version": "2.0.0-rc.2" }
            ]}]}"#,
        )
        .unwrap();
        let plugins = plugins(&index, "en");
        assert_eq!(plugins[0]["version"], "2.0.0-rc.3");
        assert_eq!(plugins[0]["installable"], true);
    }

    #[test]
    fn a_declared_body_over_the_cap_is_refused() {
        assert!(fits(None).is_ok());
        assert!(fits(Some(MAX_BYTES)).is_ok());
        assert!(fits(Some(MAX_BYTES + 1)).is_err());
        // A chunked body (no declared length) is refused as it arrives.
        let mut body = vec![0; MAX_BYTES as usize];
        assert!(push(&mut body, b"x").is_err());
        let mut body = Vec::new();
        assert!(push(&mut body, &vec![0; MAX_BYTES as usize]).is_ok());
        assert!(push(&mut body, b"x").is_err());
    }

    #[test]
    fn urls_outside_http_are_refused_and_credentials_and_fragments_dropped() {
        for url in [
            "ftp://index.invalid/i.json",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "not a url",
            "",
        ] {
            assert!(sanitized(url).is_err(), "{url}");
        }
        let url = sanitized("https://user:pw@index.invalid/i.json?x=1#frag").unwrap();
        assert_eq!(url.as_str(), "https://index.invalid/i.json?x=1");
    }

    #[test]
    fn params_needs_a_url_and_defaults_the_locale() {
        for params in [json!({}), json!({ "url": 7 }), json!({ "url": "  " })] {
            assert!(matches!(params(&params), Err(RpcError::BadParams(_))), "{params}");
        }
        assert_eq!(
            params(&json!({ "url": "https://index.invalid/i.json" })).unwrap(),
            ("https://index.invalid/i.json", "en")
        );
        assert_eq!(
            params(&json!({ "url": "https://index.invalid/i.json", "locale": "zh-Hans" })).unwrap(),
            ("https://index.invalid/i.json", "zh-Hans")
        );
    }
}
