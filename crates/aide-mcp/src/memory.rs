//! Per-project memory store: small markdown files under
//! `~/.aide/memory/<repo_slug>/<name>.md`. Each file carries YAML
//! frontmatter (`name`, optional `description`) so list-style queries
//! can return one-line summaries without reading every body.
//!
//! Mirrors the Claude Code host-level memory layout from CLAUDE.md so
//! agents that already understand that shape can switch transports
//! without learning a new format. For Claude Code itself this
//! duplicates host memory; the point of exposing it through MCP is
//! portability for Codex / Cursor / custom agents that lack a
//! built-in layer.

use std::fs;
use std::path::Path;

use regex::Regex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("memory name `{0}` is invalid — must be non-empty, no `/` or `..`, no leading dot")]
    InvalidName(String),
    #[error("memory `{0}` does not exist")]
    NotFound(String),
    #[error("memory `{0}` already exists")]
    AlreadyExists(String),
    #[error("regex pattern is invalid: {0}")]
    BadRegex(#[from] regex::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// One memory's parsed view — frontmatter fields plus the markdown body.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Memory {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub content: String,
}

/// Lightweight summary returned by `list` — frontmatter only, no body.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemorySummary {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Bytes on disk including frontmatter — quick triage signal.
    pub size_bytes: u64,
}

/// Validate a caller-supplied memory name. Rejects anything that would
/// let an arbitrary `name` escape the per-project directory or
/// trample on dotfiles. Returns the canonical filename (`<name>.md`).
pub fn name_to_filename(name: &str) -> Result<String, MemoryError> {
    if name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Err(MemoryError::InvalidName(name.to_string()));
    }
    Ok(format!("{name}.md"))
}

/// Write (or overwrite) a memory. `description` is optional but
/// recommended — `list` returns it without reading the body, so it
/// powers the "is this the memory I want?" filter.
pub fn write(
    dir: &Path,
    name: &str,
    description: Option<&str>,
    content: &str,
) -> Result<(), MemoryError> {
    let file = name_to_filename(name)?;
    fs::create_dir_all(dir)?;
    let payload = render(name, description, content);
    fs::write(dir.join(file), payload)?;
    Ok(())
}

/// Read a memory, parsing the frontmatter into structured fields.
pub fn read(dir: &Path, name: &str) -> Result<Memory, MemoryError> {
    let file = name_to_filename(name)?;
    let path = dir.join(file);
    let raw = fs::read_to_string(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => MemoryError::NotFound(name.to_string()),
        _ => MemoryError::Io(e),
    })?;
    Ok(parse(name, &raw))
}

/// List every memory under `dir`, sorted by name.
pub fn list(dir: &Path) -> Result<Vec<MemorySummary>, MemoryError> {
    let mut out = Vec::new();
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(MemoryError::Io(e)),
    };
    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(name) = file_name.strip_suffix(".md") else {
            continue;
        };
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let raw = fs::read_to_string(&path)?;
        let parsed = parse(name, &raw);
        out.push(MemorySummary {
            name: parsed.name,
            description: parsed.description,
            size_bytes: meta.len(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Delete a memory. Returns `NotFound` when there's nothing to remove.
pub fn delete(dir: &Path, name: &str) -> Result<(), MemoryError> {
    let file = name_to_filename(name)?;
    let path = dir.join(file);
    fs::remove_file(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => MemoryError::NotFound(name.to_string()),
        _ => MemoryError::Io(e),
    })?;
    Ok(())
}

/// Rename a memory. Refuses to overwrite an existing target — caller
/// must delete first if they really want that.
pub fn rename(dir: &Path, old_name: &str, new_name: &str) -> Result<(), MemoryError> {
    let old_file = name_to_filename(old_name)?;
    let new_file = name_to_filename(new_name)?;
    let old_path = dir.join(&old_file);
    let new_path = dir.join(&new_file);
    if !old_path.exists() {
        return Err(MemoryError::NotFound(old_name.to_string()));
    }
    if new_path.exists() {
        return Err(MemoryError::AlreadyExists(new_name.to_string()));
    }
    fs::rename(&old_path, &new_path)?;

    // Sync the `name:` frontmatter field with the new filename.
    let raw = fs::read_to_string(&new_path)?;
    let parsed = parse(new_name, &raw);
    let payload = render(new_name, parsed.description.as_deref(), &parsed.content);
    fs::write(&new_path, payload)?;

    Ok(())
}

/// Replace every regex match of `pattern` in the body with
/// `replacement`. Returns the number of substitutions performed.
/// Frontmatter is preserved untouched.
pub fn edit(
    dir: &Path,
    name: &str,
    pattern: &str,
    replacement: &str,
) -> Result<usize, MemoryError> {
    let file = name_to_filename(name)?;
    let path = dir.join(file);
    let raw = fs::read_to_string(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => MemoryError::NotFound(name.to_string()),
        _ => MemoryError::Io(e),
    })?;
    let parsed = parse(name, &raw);
    let regex = Regex::new(pattern)?;
    let count = regex.find_iter(&parsed.content).count();
    let new_body = regex.replace_all(&parsed.content, replacement).to_string();
    let payload = render(name, parsed.description.as_deref(), &new_body);
    fs::write(&path, payload)?;
    Ok(count)
}

/// Render a memory file to its on-disk form. `name` is authoritative
/// over whatever was in the frontmatter previously — callers can't
/// drift the filename and the `name:` field apart.
fn render(name: &str, description: Option<&str>, content: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(content.len() + 128);
    out.push_str("---\n");
    let _ = writeln!(out, "name: {name}");
    if let Some(desc) = description {
        let _ = writeln!(out, "description: {desc}");
    }
    out.push_str("---\n");
    if !content.starts_with('\n') {
        out.push('\n');
    }
    out.push_str(content);
    if !content.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Parse the on-disk form back into structured data. The parser is
/// deliberately minimal — only `name:` and `description:` frontmatter
/// keys are recognised; everything else passes through as body.
/// Files without frontmatter are accepted and reported with the
/// passed-in `default_name` plus `None` description.
fn parse(default_name: &str, raw: &str) -> Memory {
    let mut name = default_name.to_string();
    let mut description = None;
    let mut body_start = 0;

    if let Some(rest) = raw.strip_prefix("---\n") {
        if let Some(end_idx) = rest.find("\n---\n") {
            let header = &rest[..end_idx];
            for line in header.lines() {
                if let Some(v) = line.strip_prefix("name:") {
                    name = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("description:") {
                    let trimmed = v.trim();
                    if !trimmed.is_empty() {
                        description = Some(trimmed.to_string());
                    }
                }
            }
            body_start = 4 + end_idx + 5; // "---\n" + header + "\n---\n"
        }
    }

    let body = raw[body_start..].trim_start_matches('\n').to_string();
    Memory {
        name,
        description,
        content: body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn invalid_names_rejected() {
        for bad in ["", ".hidden", "foo/bar", "../escape", "back\\slash"] {
            assert!(
                matches!(name_to_filename(bad), Err(MemoryError::InvalidName(_))),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempdir().unwrap();
        write(dir.path(), "my-note", Some("a short note"), "hello\nworld").unwrap();
        let got = read(dir.path(), "my-note").unwrap();
        assert_eq!(got.name, "my-note");
        assert_eq!(got.description.as_deref(), Some("a short note"));
        assert_eq!(got.content, "hello\nworld\n");
    }

    #[test]
    fn list_summarises_without_body() {
        let dir = tempdir().unwrap();
        write(dir.path(), "alpha", Some("first"), "body one").unwrap();
        write(dir.path(), "beta", None, "body two").unwrap();
        let summaries = list(dir.path()).unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].name, "alpha");
        assert_eq!(summaries[0].description.as_deref(), Some("first"));
        assert_eq!(summaries[1].name, "beta");
        assert!(summaries[1].description.is_none());
    }

    #[test]
    fn list_empty_dir_is_ok() {
        let dir = tempdir().unwrap();
        assert!(list(dir.path()).unwrap().is_empty());
        // Non-existent dir also empty, not an error.
        assert!(list(&dir.path().join("nope")).unwrap().is_empty());
    }

    #[test]
    fn list_skips_non_md_files() {
        let dir = tempdir().unwrap();
        write(dir.path(), "real", None, "ok").unwrap();
        fs::write(dir.path().join("README.txt"), "not a memory").unwrap();
        let summaries = list(dir.path()).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "real");
    }

    #[test]
    fn rename_updates_name_field_inside_file() {
        let dir = tempdir().unwrap();
        write(dir.path(), "old", Some("d"), "body").unwrap();
        rename(dir.path(), "old", "new").unwrap();
        assert!(!dir.path().join("old.md").exists());
        let got = read(dir.path(), "new").unwrap();
        assert_eq!(got.name, "new");
    }

    #[test]
    fn rename_refuses_to_overwrite() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a", None, "1").unwrap();
        write(dir.path(), "b", None, "2").unwrap();
        assert!(matches!(
            rename(dir.path(), "a", "b"),
            Err(MemoryError::AlreadyExists(_))
        ));
    }

    #[test]
    fn delete_missing_returns_not_found() {
        let dir = tempdir().unwrap();
        assert!(matches!(
            delete(dir.path(), "ghost"),
            Err(MemoryError::NotFound(_))
        ));
    }

    #[test]
    fn edit_replaces_only_body() {
        let dir = tempdir().unwrap();
        write(dir.path(), "n", Some("desc"), "foo bar foo baz").unwrap();
        let count = edit(dir.path(), "n", r"foo", "XYZ").unwrap();
        assert_eq!(count, 2);
        let got = read(dir.path(), "n").unwrap();
        assert_eq!(got.content, "XYZ bar XYZ baz\n");
        assert_eq!(got.description.as_deref(), Some("desc"));
    }

    #[test]
    fn parse_files_without_frontmatter() {
        let raw = "just a body\nwith two lines\n";
        let m = parse("fallback", raw);
        assert_eq!(m.name, "fallback");
        assert!(m.description.is_none());
        assert_eq!(m.content, raw);
    }

    #[test]
    fn read_missing_returns_not_found() {
        let dir = tempdir().unwrap();
        assert!(matches!(
            read(dir.path(), "ghost"),
            Err(MemoryError::NotFound(_))
        ));
    }
}
