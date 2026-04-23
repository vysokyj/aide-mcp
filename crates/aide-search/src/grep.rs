//! Regex search across a scope-selected file list.
//!
//! Uses `grep-regex` + `grep-searcher` (the engine ripgrep is built on):
//! - Smart-case matching by default (lowercase pattern → case-insensitive;
//!   mixed-case → case-sensitive).
//! - Binary files are skipped by default (null-byte detection).
//! - Optional `before`/`after` context lines are returned alongside
//!   match lines with an explicit `kind` discriminator.
//!
//! Hits are bounded by both `max_results` (total across files) and
//! `max_results_per_file` to prevent a single noisy file from dominating
//! the response.

use std::path::Path;

use grep_matcher::LineTerminator;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch};
use serde::{Deserialize, Serialize};

use crate::{ls::list_files, LsOptions, Scope, SearchError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepOptions {
    /// Glob filter over repo-relative paths (applied before search).
    pub glob: Option<String>,

    /// `None` = auto / smart-case. `Some(true)` = case-sensitive,
    /// `Some(false)` = case-insensitive regardless of pattern casing.
    pub case_sensitive: Option<bool>,

    /// Lines of context before each match (capped at 10).
    pub before_context: usize,

    /// Lines of context after each match (capped at 10).
    pub after_context: usize,

    /// Per-file cap on matches to keep (excluding context lines).
    pub max_results_per_file: usize,

    /// Total cap on matches across all files (excluding context lines).
    pub max_results: usize,

    /// Hidden files (dotfiles). Only affects `Scope::All`.
    pub include_hidden: bool,
}

impl Default for GrepOptions {
    fn default() -> Self {
        Self {
            glob: None,
            case_sensitive: None,
            before_context: 0,
            after_context: 0,
            max_results_per_file: 50,
            max_results: 200,
            include_hidden: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepHit {
    pub path: String,
    pub lines: Vec<LineMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineMatch {
    pub line: u64,
    pub text: String,
    pub kind: LineKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Match,
    Before,
    After,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepResult {
    pub hits: Vec<GrepHit>,
    pub files_scanned: usize,
    pub total_matches: usize,
    pub truncated: bool,
}

/// Run a regex search over `scope` inside `repo_root`.
pub fn grep(
    repo_root: &Path,
    pattern: &str,
    scope: &Scope,
    options: &GrepOptions,
) -> Result<GrepResult, SearchError> {
    let matcher = build_matcher(pattern, options.case_sensitive)?;
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(options.before_context.min(10))
        .after_context(options.after_context.min(10))
        .line_terminator(LineTerminator::byte(b'\n'))
        .binary_detection(grep_searcher::BinaryDetection::quit(0))
        .build();

    let ls_opts = LsOptions {
        glob: options.glob.clone(),
        max_results: None,
        include_hidden: options.include_hidden,
    };
    let files = list_files(repo_root, scope, &ls_opts)?;

    let mut hits: Vec<GrepHit> = Vec::new();
    let mut total_matches = 0usize;
    let mut truncated = false;
    let files_scanned = files.len();

    'files: for rel in &files {
        let abs = repo_root.join(rel);
        let remaining = options.max_results.saturating_sub(total_matches);
        if remaining == 0 {
            truncated = true;
            break 'files;
        }
        let effective_cap = options.max_results_per_file.min(remaining);
        let mut sink = CollectSink {
            path: rel,
            lines: Vec::new(),
            match_count: 0,
            per_file_cap: effective_cap,
        };
        // Ignore per-file errors — unreadable file, bad UTF-8, etc.
        // should not abort a bulk search.
        let _ = searcher.search_path(&matcher, &abs, &mut sink);

        if sink.lines.is_empty() {
            continue;
        }
        total_matches = total_matches.saturating_add(sink.match_count);

        hits.push(GrepHit {
            path: rel.clone(),
            lines: sink.lines,
        });

        if total_matches >= options.max_results {
            truncated = true;
            break 'files;
        }
    }

    Ok(GrepResult {
        hits,
        files_scanned,
        total_matches,
        truncated,
    })
}

fn build_matcher(pattern: &str, case_sensitive: Option<bool>) -> Result<RegexMatcher, SearchError> {
    let mut b = RegexMatcherBuilder::new();
    match case_sensitive {
        Some(true) => {
            b.case_insensitive(false).case_smart(false);
        }
        Some(false) => {
            b.case_insensitive(true).case_smart(false);
        }
        None => {
            b.case_smart(true);
        }
    }
    b.build(pattern)
        .map_err(|source| SearchError::InvalidRegex {
            pattern: pattern.to_string(),
            source,
        })
}

struct CollectSink<'a> {
    path: &'a str,
    lines: Vec<LineMatch>,
    match_count: usize,
    per_file_cap: usize,
}

impl Sink for CollectSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if self.match_count >= self.per_file_cap {
            return Ok(false);
        }
        let line = mat.line_number().unwrap_or(0);
        let text = strip_eol(mat.bytes());
        self.lines.push(LineMatch {
            line,
            text,
            kind: LineKind::Match,
        });
        self.match_count += 1;
        Ok(true)
    }

    fn context(&mut self, _: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, Self::Error> {
        let line = ctx.line_number().unwrap_or(0);
        let kind = match ctx.kind() {
            SinkContextKind::Before => LineKind::Before,
            SinkContextKind::After => LineKind::After,
            SinkContextKind::Other => return Ok(true),
        };
        let text = strip_eol(ctx.bytes());
        self.lines.push(LineMatch { line, text, kind });
        Ok(true)
    }
}

impl CollectSink<'_> {
    #[allow(
        dead_code,
        reason = "path is useful for tracing; keep for future debug logging"
    )]
    fn path(&self) -> &str {
        self.path
    }
}

fn strip_eol(bytes: &[u8]) -> String {
    let trimmed = bytes
        .strip_suffix(b"\n")
        .unwrap_or(bytes)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| bytes.strip_suffix(b"\n").unwrap_or(bytes));
    String::from_utf8_lossy(trimmed).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn init_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        let repo = git2::Repository::init(&path).unwrap();
        fs::write(
            path.join("a.rs"),
            "fn hello() {}\nlet x = 1;\nfn world() {}\n",
        )
        .unwrap();
        fs::write(
            path.join("b.rs"),
            "// HELLO comment\nfn b() {}\nfn goodbye() {}\n",
        )
        .unwrap();
        fs::write(path.join("bin.dat"), [0u8, 1, 2, 3, 4]).unwrap();
        let mut index = repo.index().unwrap();
        for p in ["a.rs", "b.rs", "bin.dat"] {
            index.add_path(Path::new(p)).unwrap();
        }
        let tree_id = index.write_tree().unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        (dir, path)
    }

    #[test]
    fn finds_literal_matches_across_files() {
        let (_d, path) = init_repo();
        let result = grep(&path, "fn hello", &Scope::Tracked, &GrepOptions::default()).unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "a.rs");
        assert_eq!(result.hits[0].lines[0].line, 1);
        assert_eq!(result.hits[0].lines[0].kind, LineKind::Match);
        assert_eq!(result.total_matches, 1);
    }

    #[test]
    fn smart_case_treats_lowercase_as_insensitive() {
        let (_d, path) = init_repo();
        let result = grep(&path, "hello", &Scope::Tracked, &GrepOptions::default()).unwrap();
        // Matches "hello" in a.rs and "HELLO" in b.rs.
        assert_eq!(result.hits.len(), 2);
    }

    #[test]
    fn smart_case_treats_mixed_case_as_sensitive() {
        let (_d, path) = init_repo();
        let result = grep(&path, "HELLO", &Scope::Tracked, &GrepOptions::default()).unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "b.rs");
    }

    #[test]
    fn explicit_case_insensitive_override_wins() {
        let (_d, path) = init_repo();
        let result = grep(
            &path,
            "HELLO",
            &Scope::Tracked,
            &GrepOptions {
                case_sensitive: Some(false),
                ..GrepOptions::default()
            },
        )
        .unwrap();
        // Should match both cases.
        assert_eq!(result.hits.len(), 2);
    }

    #[test]
    fn skips_binary_files() {
        let (_d, path) = init_repo();
        // Pattern that could appear in bin.dat bytes (unlikely but
        // the point is the binary-detection skip kicks in).
        let result = grep(&path, ".", &Scope::Tracked, &GrepOptions::default()).unwrap();
        assert!(result.hits.iter().all(|h| h.path != "bin.dat"));
    }

    #[test]
    fn glob_filter_restricts_files() {
        let (_d, path) = init_repo();
        let result = grep(
            &path,
            "fn",
            &Scope::Tracked,
            &GrepOptions {
                glob: Some("a.rs".into()),
                ..GrepOptions::default()
            },
        )
        .unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "a.rs");
    }

    #[test]
    fn returns_context_lines_with_kinds() {
        let (_d, path) = init_repo();
        let result = grep(
            &path,
            "x = 1",
            &Scope::Tracked,
            &GrepOptions {
                before_context: 1,
                after_context: 1,
                ..GrepOptions::default()
            },
        )
        .unwrap();
        let hit = &result.hits[0];
        let kinds: Vec<LineKind> = hit.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![LineKind::Before, LineKind::Match, LineKind::After]
        );
    }

    #[test]
    fn max_results_truncates_and_reports() {
        let (_d, path) = init_repo();
        let result = grep(
            &path,
            "fn",
            &Scope::Tracked,
            &GrepOptions {
                max_results: 1,
                ..GrepOptions::default()
            },
        )
        .unwrap();
        assert!(result.truncated);
        assert_eq!(result.total_matches, 1);
    }

    #[test]
    fn per_file_cap_applies_before_global_cap() {
        let (_d, path) = init_repo();
        let result = grep(
            &path,
            "fn",
            &Scope::Tracked,
            &GrepOptions {
                max_results_per_file: 1,
                max_results: 100,
                ..GrepOptions::default()
            },
        )
        .unwrap();
        // a.rs has "fn hello" and "fn world" but should report only 1.
        let a_hit = result.hits.iter().find(|h| h.path == "a.rs").unwrap();
        assert_eq!(a_hit.lines.len(), 1);
    }
}
