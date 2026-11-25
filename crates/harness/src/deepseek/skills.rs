//! The workspace skills this scanner reads from disk (SUR-56).
//!
//! dsh's filesystem provider scans four default roots plus a configurable
//! custom list (empty by default) and its bundled catalog; this scanner
//! covers three of the roots in rank order — project `.dsh`, project
//! `.agents`, and the harness home's `skills`. It accepts a
//! skill as a `<root>/<name>/SKILL.md` bundle or a flat `<root>/<name>.md`,
//! one level deep. Reading the same files here means the composer's `$` menu
//! can offer them before a chat starts, with no runtime to ask.

use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
};

use zeron_proto::invocation::Skill;

/// The skills this scanner (SUR-56) reads for `cwd` — three of the roots
/// dsh's provider scans, not all it loads — sorted by name, the first root
/// winning a duplicate (project `.dsh`, project `.agents`, then the harness
/// home's `skills`). An unreadable root or file is skipped, never fatal: one
/// bad entry must not hide the rest of the menu.
pub fn discover(cwd: &Path, home: &Path) -> Vec<Skill> {
    let cwd = std::path::absolute(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let project = project_root(&cwd);
    // The home root keeps dsh's own bundled catalog in `.system`; it is not
    // ours to offer alongside the skills a person wrote.
    let roots = [
        (project.join(".dsh").join("skills"), false),
        (project.join(".agents").join("skills"), false),
        (home.join("skills"), true),
    ];
    let mut seen = BTreeSet::new();
    let mut skills = Vec::new();
    for (root, skip_system) in roots {
        for skill in scan(&root, skip_system) {
            if seen.insert(skill.name.clone()) {
                skills.push(skill);
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// The nearest ancestor holding a `.git` marker — a directory in a clone, a
/// file in a worktree — else `cwd` itself, as dsh resolves a project.
fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// One root's direct entries: a directory is a bundle with a `SKILL.md`
/// inside it, a flat file is itself. Sorted, because the filesystem's own
/// order is not stable enough to show a person.
fn scan(root: &Path, skip_system: bool) -> Vec<Skill> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        if skip_system && entry.file_name() == ".system" {
            continue;
        }
        let path = entry.path();
        // Follow a symlink to decide what it is, as dsh's stat does.
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            let bundle = path.join("SKILL.md");
            if bundle.is_file() {
                files.push(bundle);
            }
        } else if meta.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    files.sort();
    files.iter().filter_map(|path| read_skill(path)).collect()
}

/// A skill file's frontmatter as this parser reads it; it is not dsh's full
/// reader, and an entry it cannot read is skipped rather than guessed.
fn read_skill(path: &Path) -> Option<Skill> {
    let raw = fs::read_to_string(path).ok()?;
    let fields = frontmatter(&raw)?;
    // dsh rejects these spellings outright rather than reading them, so a
    // skill carrying one never loads there.
    if fields.iter().any(|(key, _)| {
        matches!(
            key.as_str(),
            "userInvocable" | "modelInvocable" | "disableModelInvocation"
        )
    }) {
        return None;
    }
    let name = field(&fields, "name")?;
    // dsh accepts only kebab-case names.
    let kebab = name.split('-').all(|part| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });
    if !kebab {
        return None;
    }
    let description = field(&fields, "description")?;
    let enabled = invocation_flag(&fields, "user-invocable")?;
    Some(Skill {
        name: name.to_string(),
        path: path.display().to_string(),
        description: description.to_string(),
        enabled,
        command: None,
    })
}

/// A frontmatter value with its YAML scalar kind kept, so `name` and
/// `description` can be held to dsh's strings-only rule while a flag like
/// `user-invocable` still reads any kind.
enum Value {
    /// Unquoted, with a trailing YAML comment already removed.
    Plain(String),
    Quoted(String),
    Block(String),
}

impl Value {
    fn text(&self) -> &str {
        match self {
            Value::Plain(text) | Value::Quoted(text) | Value::Block(text) => text,
        }
    }
}

/// The leading spaces/tabs on a frontmatter line. A key nested deeper than
/// the root mapping is part of the key above it, not a top-level field.
fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| matches!(c, ' ' | '\t')).count()
}

/// The `key: value` frontmatter between the first two `---` lines. Only keys
/// at the root mapping's own indent are fields; a more- or less-indented line
/// belongs to the entry above it and is skipped. Quoted scalars are unwrapped
/// and `|`/`>` block scalars are joined onto one line: a menu row shows a
/// sentence, not a document.
fn frontmatter(raw: &str) -> Option<Vec<(String, Value)>> {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.first().copied()? != "---" {
        return None;
    }
    let closing = lines.iter().skip(1).position(|line| *line == "---")? + 1;
    let mut fields = Vec::new();
    let mut index = 1;
    // The root mapping's indent, from its first key: usually none, but a
    // document may indent its whole root mapping.
    let mut base: Option<usize> = None;
    // yaml rejects duplicate keys in every mapping, not only in the fields
    // this scanner reads. Track the key path so sibling mappings may reuse a
    // key while two keys in one mapping cannot.
    let mut seen_keys = HashSet::new();
    let mut parents: Vec<(usize, String)> = Vec::new();
    while index < closing {
        let line = lines[index];
        index += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Tabs may separate or comment a line, but they may not be YAML
        // indentation; yaml reports the ubiquitous-tab error and rejects the
        // document.
        if line[..indent_of(line)].contains('\t') {
            return None;
        }
        let indent = indent_of(line);
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim().to_string();
        while parents
            .last()
            .is_some_and(|(parent_indent, _)| *parent_indent >= indent)
        {
            parents.pop();
        }
        let mut path: Vec<String> = parents.iter().map(|(_, key)| key.clone()).collect();
        path.push(key.clone());
        if !seen_keys.insert(path) {
            return None;
        }
        parents.push((indent, key.clone()));
        let value = value.trim();
        let Some(marker) = value.chars().next().filter(|c| matches!(c, '|' | '>')) else {
            let scalar = scalar(value)?;
            let base = *base.get_or_insert(indent);
            if indent == base {
                fields.push((key, scalar));
            }
            continue;
        };
        let base = *base.get_or_insert(indent);
        // A block scalar's lines are the more-indented ones that follow; its
        // chomping indicator only affects a trailing break nobody displays.
        let mut parts = Vec::new();
        while index < closing {
            let next = lines[index];
            if !next.trim().is_empty() && indent_of(next) <= base {
                break;
            }
            index += 1;
            parts.push(next.trim());
        }
        let text = if marker == '>' {
            parts.join(" ")
        } else {
            parts.join("\n")
        };
        if indent == base {
            fields.push((key, Value::Block(text.trim().to_string())));
        }
    }
    Some(fields)
}

/// A single-line scalar: a value wrapped in matching quotes stays a string
/// under the existing unquote rules, and a comment after the closing quote
/// is dropped; anything else is plain. A quote that never closes, or one
/// followed by anything but space and a comment, makes the YAML document
/// malformed rather than a differently shaped scalar.
fn scalar(value: &str) -> Option<Value> {
    if matches!(value.as_bytes().first(), Some(b'"' | b'\'')) {
        let end = quoted_end(value)?;
        let rest = value[end..].trim_start();
        if rest.is_empty() || rest.starts_with('#') {
            return Some(Value::Quoted(unquote(&value[..end])));
        }
        return None;
    }
    Some(Value::Plain(strip_comment(value)))
}

/// The byte index just past an opening quote's matching close, or `None`
/// when `value` does not open with a quote or the quote never closes. Inside
/// `"…"` a backslash escapes the byte after it; inside `'…'` a doubled `''`
/// is an escaped quote, not the close.
fn quoted_end(value: &str) -> Option<usize> {
    let quote = match value.as_bytes().first()? {
        &quote @ (b'"' | b'\'') => quote,
        _ => return None,
    };
    let bytes = value.as_bytes();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if quote == b'"' => index += 2,
            byte if byte == quote => {
                if quote == b'\'' && bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                } else {
                    return Some(index + 1);
                }
            }
            _ => index += 1,
        }
    }
    None
}

/// A plain scalar ends at the first `#` that starts the value or follows a
/// space or tab; a `#` inside a word is text.
fn strip_comment(value: &str) -> String {
    let bytes = value.as_bytes();
    let cut = (0..bytes.len())
        .find(|&i| bytes[i] == b'#' && (i == 0 || matches!(bytes[i - 1], b' ' | b'\t')));
    match cut {
        Some(i) => value[..i]
            .trim_end_matches(|c| c == ' ' || c == '\t')
            .to_string(),
        None => value.to_string(),
    }
}

fn field<'a>(fields: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(field, _)| field == key)
        .and_then(|(_, value)| match value {
            Value::Plain(text) if !plain_is_string(text) => None,
            _ => Some(value.text()),
        })
        .filter(|value| !value.is_empty())
}

/// Whether a plain scalar is a YAML string rather than a null, bool, or
/// number — the kinds dsh's `stringField` check drops are not offered here.
fn plain_is_string(text: &str) -> bool {
    if text.is_empty()
        || matches!(
            text,
            "~" | "null" | "Null" | "NULL" | "true" | "True" | "TRUE" | "false" | "False" | "FALSE"
        )
    {
        return false;
    }
    !plain_is_number(text)
}

/// The YAML number spellings: unsigned `0x` hex and `0o` octal, and an
/// optional sign then digits with an optional fraction or exponent, or the
/// `.inf`/`.nan` spellings.
fn plain_is_number(text: &str) -> bool {
    if let Some(hex) = text.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    if let Some(oct) = text.strip_prefix("0o") {
        return !oct.is_empty() && oct.bytes().all(|b| matches!(b, b'0'..=b'7'));
    }
    let body = text
        .strip_prefix('+')
        .or_else(|| text.strip_prefix('-'))
        .unwrap_or(text);
    if body.eq_ignore_ascii_case(".inf") || body.eq_ignore_ascii_case(".nan") {
        return true;
    }
    let (mantissa, exponent) = match body.find(|c| c == 'e' || c == 'E') {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let mut digits = 0;
    let mut dots = 0;
    for byte in mantissa.bytes() {
        match byte {
            b'0'..=b'9' => digits += 1,
            b'.' => dots += 1,
            _ => return false,
        }
    }
    if digits == 0 || dots > 1 {
        return false;
    }
    match exponent {
        None => true,
        Some(exp) => {
            let exp = exp
                .strip_prefix('+')
                .or_else(|| exp.strip_prefix('-'))
                .unwrap_or(exp);
            !exp.is_empty() && exp.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

/// `user-invocable` in dsh's boolean grammar: omitted permits the surface,
/// the spellings it accepts decide, and one it rejects drops the skill.
fn invocation_flag(fields: &[(String, Value)], key: &str) -> Option<bool> {
    let Some(value) = fields
        .iter()
        .find(|(field, _)| field == key)
        .map(|(_, value)| value.text().to_ascii_lowercase())
    else {
        return Some(true);
    };
    match value.as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn unquote(value: &str) -> String {
    if let Some(inner) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        return inner.replace("\\\"", "\"").replace("\\n", " ");
    }
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return inner.replace("''", "'");
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    /// A clone of a repository with a `.git` marker and a `src` folder.
    fn project(dir: &Path) -> PathBuf {
        let root = dir.join("project");
        fs::create_dir_all(root.join(".git")).unwrap();
        root
    }

    fn names(skills: &[Skill]) -> Vec<&str> {
        skills.iter().map(|skill| skill.name.as_str()).collect()
    }

    #[test]
    fn a_bundle_and_a_flat_file_are_both_skills() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        write(
            &root.join(".dsh/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review the diff\n---\n\nBody.\n",
        );
        write(
            &root.join(".agents/skills/plan.md"),
            "---\nname: plan\ndescription: Plan the work\n---\n",
        );
        let skills = discover(&root.join("src"), &dir.path().join("home"));
        assert_eq!(names(&skills), ["plan", "review"]);
        let review = skills.iter().find(|s| s.name == "review").unwrap();
        assert_eq!(review.description, "Review the diff");
        // Joined, not concatenated: a `/`-separated string keeps its
        // separators through `display()`, while `discover` returns the native
        // ones (SUR-157).
        assert_eq!(
            Path::new(&review.path),
            root.join(".dsh")
                .join("skills")
                .join("review")
                .join("SKILL.md")
                .as_path()
        );
        assert!(skills.iter().all(|s| s.enabled && s.command.is_none()));
    }

    #[test]
    fn the_project_root_wins_a_duplicate_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        write(
            &root.join(".dsh/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Project review\n---\n",
        );
        write(
            &root.join(".agents/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Agents review\n---\n",
        );
        write(
            &home.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Home review\n---\n",
        );
        let skills = discover(&root, &home);
        assert_eq!(names(&skills), ["review"]);
        assert_eq!(skills[0].description, "Project review");
    }

    #[test]
    fn a_file_without_a_name_or_description_is_not_a_skill() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        write(
            &home.join("skills/no-description.md"),
            "---\nname: nameless\n---\n",
        );
        write(&home.join("skills/no-frontmatter.md"), "# A heading\n");
        write(&home.join("skills/empty.md"), "");
        assert!(discover(&root, &home).is_empty());
    }

    #[test]
    fn user_invocable_false_is_listed_switched_off() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        write(
            &home.join("skills/quiet/SKILL.md"),
            "---\nname: quiet\ndescription: Quiet\nuser-invocable: no\n---\n",
        );
        write(
            &home.join("skills/loud/SKILL.md"),
            "---\nname: loud\ndescription: Loud\nuser-invocable: yes\n---\n",
        );
        // A spelling dsh rejects drops the skill rather than guessing.
        write(
            &home.join("skills/broken/SKILL.md"),
            "---\nname: broken\ndescription: Broken\nuser-invocable: maybe\n---\n",
        );
        let skills = discover(&root, &home);
        assert_eq!(names(&skills), ["loud", "quiet"]);
        assert!(skills[0].enabled);
        assert!(
            !skills[1].enabled,
            "user-invocable: no is listed, not offered"
        );
    }

    #[test]
    fn the_home_roots_system_child_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        write(
            &home.join("skills/.system/hidden/SKILL.md"),
            "---\nname: hidden\ndescription: Bundled\n---\n",
        );
        write(
            &home.join("skills/.system/flat.md"),
            "---\nname: bundled\ndescription: Bundled\n---\n",
        );
        write(
            &home.join("skills/visible.md"),
            "---\nname: visible\ndescription: Mine\n---\n",
        );
        assert_eq!(names(&discover(&root, &home)), ["visible"]);
    }

    #[test]
    fn a_worktree_git_file_is_a_project_marker() {
        let dir = tempfile::tempdir().unwrap();
        let clone = dir.path().join("clone");
        fs::create_dir_all(clone.join(".git")).unwrap();
        write(
            &clone.join(".dsh/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Outer\n---\n",
        );
        let worktree = clone.join("wt");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: ../.git/worktrees/wt\n").unwrap();
        write(
            &worktree.join(".dsh/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Inner worktree\n---\n",
        );
        write(
            &worktree.join(".dsh/skills/tidy/SKILL.md"),
            "---\nname: tidy\ndescription: Tidy\n---\n",
        );
        let skills = discover(&worktree.join("src/deep"), &dir.path().join("home"));
        // The nearer marker wins: `Inner worktree` beats the clone's skill.
        assert_eq!(names(&skills), ["review", "tidy"]);
        assert_eq!(skills[0].description, "Inner worktree");
    }

    #[test]
    fn a_quoted_or_folded_description_reaches_the_menu() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        write(
            &home.join("skills/quoted/SKILL.md"),
            "---\nname: quoted\ndescription: \"Review: the diff, then stop\"\n---\n",
        );
        write(
            &home.join("skills/folded/SKILL.md"),
            "---\nname: folded\ndescription: >\n  Review the pending\n  diff before merge\n---\n",
        );
        let skills = discover(&root, &home);
        let description = |name| {
            skills
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .description
                .clone()
        };
        assert_eq!(description("quoted"), "Review: the diff, then stop");
        assert_eq!(
            description("folded"),
            "Review the pending diff before merge"
        );
    }

    #[test]
    fn a_name_dsh_rejects_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        write(
            &home.join("skills/upper.md"),
            "---\nname: Review It\ndescription: Nope\n---\n",
        );
        assert!(discover(&root, &home).is_empty());
    }

    #[test]
    fn a_legacy_camel_case_invocation_key_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        // dsh rejects the camelCase spellings of the invocation fields rather
        // than reading them, so a skill carrying one never loads there and
        // must not be offered here.
        write(
            &home.join("skills/legacy/SKILL.md"),
            "---\nname: legacy\ndescription: Legacy\nuserInvocable: false\n---\n",
        );
        write(
            &home.join("skills/legacy-model/SKILL.md"),
            "---\nname: legacy-model\ndescription: Legacy model\nmodelInvocable: true\n---\n",
        );
        write(
            &home.join("skills/legacy-disable/SKILL.md"),
            "---\nname: legacy-disable\ndescription: Legacy disable\ndisableModelInvocation: false\n---\n",
        );
        assert!(discover(&root, &home).is_empty());
    }

    #[test]
    fn a_name_or_description_that_is_not_a_string_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        // YAML reads these as a number and a boolean; dsh requires strings and
        // drops the skill, so it is not offered here either.
        write(
            &home.join("skills/number-name/SKILL.md"),
            "---\nname: 123\ndescription: Numeric name\n---\n",
        );
        write(
            &home.join("skills/boolean-description/SKILL.md"),
            "---\nname: boolean-description\ndescription: true\n---\n",
        );
        assert!(discover(&root, &home).is_empty());
    }

    #[test]
    fn an_inline_comment_is_not_shown_in_a_description() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        // dsh's YAML parse ends a plain scalar at a `#` that follows a space;
        // the menu shows the same sentence it would.
        write(
            &home.join("skills/commented/SKILL.md"),
            "---\nname: commented\ndescription: Review the diff # keep in step with dsh\n---\n",
        );
        // A quoted `#` is text, not a comment.
        write(
            &home.join("skills/quoted/SKILL.md"),
            "---\nname: quoted\ndescription: \"Review #1 first\"\n---\n",
        );
        // A comment after a quoted scalar is still a comment.
        write(
            &home.join("skills/quoted-commented/SKILL.md"),
            "---\nname: quoted-commented\ndescription: \"Review #2 first\" # keep\n---\n",
        );
        let skills = discover(&root, &home);
        assert_eq!(names(&skills), ["commented", "quoted", "quoted-commented"]);
        let description = |name: &str| {
            skills
                .iter()
                .find(|skill| skill.name == name)
                .unwrap()
                .description
                .as_str()
        };
        assert_eq!(description("commented"), "Review the diff");
        assert_eq!(description("quoted"), "Review #1 first");
        assert_eq!(description("quoted-commented"), "Review #2 first");
    }

    #[test]
    fn a_nested_invocation_key_is_not_a_top_level_field() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        // `metadata` is a nested mapping: its `userInvocable` is not the
        // top-level field dsh rejects, so the skill stays.
        write(
            &home.join("skills/nested/SKILL.md"),
            "---\nname: nested\ndescription: Nested\nmetadata:\n  userInvocable: false\n---\n",
        );
        assert_eq!(names(&discover(&root, &home)), ["nested"]);
    }

    #[test]
    fn a_nested_name_is_not_the_top_level_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        // The nested `name: 123` belongs to `metadata`; the top-level name is
        // `foo`, so the skill is offered as foo (dsh reads the same).
        write(
            &home.join("skills/nested/SKILL.md"),
            "---\nmetadata:\n  name: 123\nname: foo\ndescription: Foo\n---\n",
        );
        let skills = discover(&root, &home);
        assert_eq!(names(&skills), ["foo"]);
    }

    #[test]
    fn a_key_inside_a_block_scalar_is_not_a_field() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(dir.path());
        let home = dir.path().join("home");
        // A block scalar's content is text, not frontmatter keys.
        write(
            &home.join("skills/block/SKILL.md"),
            "---\nname: block\ndescription: |\n  userInvocable: false\n---\n",
        );
        assert_eq!(names(&discover(&root, &home)), ["block"]);
    }

    #[test]
    fn malformed_yaml_is_accepted_exactly_when_the_sdk_accepts_it() {
        // The `sdk_accepts` column was measured against the pinned SDK's
        // yaml@2.9.1 `parse`: this table is the acceptance boundary, not a
        // list of shapes this parser happens to like.
        let cases = [
            (
                "plain mapping",
                "name: review\ndescription: Review the diff\n",
                true,
            ),
            (
                "tab-indented root key",
                "name: review\n\tdescription: Review the diff\n",
                false,
            ),
            (
                "tab-indented nested key",
                "name: review\nmetadata:\n\tdescription: Review the diff\n",
                false,
            ),
            (
                "tab-indented comment",
                "# comment\n\t# indented comment\nname: review\ndescription: Review\n",
                true,
            ),
            (
                "trailing junk after a quoted scalar",
                "name: review\ndescription: \"Review the diff\" trailing\n",
                false,
            ),
            (
                "unterminated quote",
                "name: review\ndescription: \"Review the diff\n",
                false,
            ),
            (
                "duplicate root keys",
                "name: review\ndescription: Review\ndescription: Duplicate\n",
                false,
            ),
            (
                "duplicate nested keys",
                "name: review\nmetadata:\n  owner: one\n  owner: two\n",
                false,
            ),
            (
                "nested mapping",
                "name: review\nmetadata:\n  owner: one\ndescription: Review\n",
                true,
            ),
            (
                "block scalar",
                "name: review\ndescription: |\n  Review the diff\n  carefully\n",
                true,
            ),
        ];
        for (name, body, sdk_accepts) in cases {
            let raw = format!("---\n{body}---\n");
            assert_eq!(frontmatter(&raw).is_some(), sdk_accepts, "{name}");
        }
    }
}
