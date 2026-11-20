//! The skills dsh loads from disk for a workspace (SUR-56).
//!
//! dsh's filesystem provider scans three roots in rank order and accepts a
//! skill as a `<root>/<name>/SKILL.md` bundle or a flat `<root>/<name>.md`,
//! one level deep. Reading the same files here means the composer's `$` menu
//! can offer them before a chat starts, with no runtime to ask.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use zeron_proto::invocation::Skill;

/// The skills dsh would load for `cwd`, sorted by name, the first root
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

/// A skill file's frontmatter as the menu needs it; anything dsh would skip
/// is skipped here too, so the composer never offers a skill a run would not
/// load.
fn read_skill(path: &Path) -> Option<Skill> {
    let raw = fs::read_to_string(path).ok()?;
    let fields = frontmatter(&raw)?;
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

/// The `key: value` frontmatter between the first two `---` lines. Quoted
/// scalars are unwrapped and `|`/`>` block scalars are joined onto one line:
/// a menu row shows a sentence, not a document.
fn frontmatter(raw: &str) -> Option<Vec<(String, String)>> {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.first().copied()? != "---" {
        return None;
    }
    let closing = lines.iter().skip(1).position(|line| *line == "---")? + 1;
    let mut fields = Vec::new();
    let mut index = 1;
    while index < closing {
        let line = lines[index];
        index += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let Some(marker) = value.chars().next().filter(|c| matches!(c, '|' | '>')) else {
            fields.push((key.trim().to_string(), unquote(value)));
            continue;
        };
        // A block scalar's lines are the more-indented ones that follow; its
        // chomping indicator only affects a trailing break nobody displays.
        let mut parts = Vec::new();
        while index < closing {
            let next = lines[index];
            if !next.trim().is_empty() && !next.starts_with(' ') && !next.starts_with('\t') {
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
        fields.push((key.trim().to_string(), text.trim().to_string()));
    }
    Some(fields)
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(field, _)| field == key)
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}

/// `user-invocable` in dsh's boolean grammar: omitted permits the surface,
/// the spellings it accepts decide, and one it rejects drops the skill.
fn invocation_flag(fields: &[(String, String)], key: &str) -> Option<bool> {
    let Some(value) = fields
        .iter()
        .find(|(field, _)| field == key)
        .map(|(_, value)| value.to_ascii_lowercase())
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
        // Compared as a path, not a string: `display()` spells the OS
        // separator, so a string expectation would assert the platform
        // rather than which file the skill was read from.
        assert_eq!(
            Path::new(&review.path),
            root.join(".dsh/skills/review/SKILL.md").as_path()
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
}
