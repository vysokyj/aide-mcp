//! Parse dogfood run files (`dogfood/runs/NNN-*.md`) and aggregate
//! their `## Coverage gaps` bullets into a ranked report. Closes the
//! loop between the paired-agent benchmark and the roadmap: the most
//! frequently recurring "aide has no equivalent for X" bullet points
//! at the next feature to build.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CoverageGap {
    /// Human-phrased missing capability — left side of `→` in the
    /// source bullet, trimmed.
    pub capability: String,
    /// Proposed tool text — right side of `→` in the first bullet
    /// that mentioned this capability. Retained verbatim so the
    /// agent sees the original phrasing.
    pub proposed_tool: Option<String>,
    pub occurrences: usize,
    pub run_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverageGapsReport {
    pub runs_scanned: usize,
    pub gaps: Vec<CoverageGap>,
}

pub fn aggregate_coverage_gaps(runs_dir: &Path) -> Result<CoverageGapsReport, String> {
    let entries = match std::fs::read_dir(runs_dir) {
        Ok(e) => e,
        Err(e) => return Err(format!("read_dir {}: {e}", runs_dir.display())),
    };

    let mut run_paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();
    run_paths.sort();

    let mut by_capability: BTreeMap<String, CoverageGap> = BTreeMap::new();
    let mut runs_scanned = 0usize;
    for path in &run_paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        runs_scanned += 1;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        for bullet in extract_coverage_gap_bullets(&text) {
            let Some((capability, proposed)) = split_arrow(&bullet) else {
                continue;
            };
            if is_non_gap_annotation(&proposed) {
                continue;
            }
            let key = capability.clone();
            let entry = by_capability.entry(key).or_insert_with(|| CoverageGap {
                capability,
                proposed_tool: Some(proposed.clone()),
                occurrences: 0,
                run_files: Vec::new(),
            });
            entry.occurrences += 1;
            if !entry.run_files.contains(&name) {
                entry.run_files.push(name.clone());
            }
        }
    }

    let mut gaps: Vec<_> = by_capability.into_values().collect();
    gaps.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then_with(|| a.capability.cmp(&b.capability))
    });
    Ok(CoverageGapsReport { runs_scanned, gaps })
}

/// Pull every bullet (`- …`) that lives under a `## Coverage gaps`
/// heading, up to the next `## ` heading. Multi-line bullets (wrapped
/// continuations) are joined into a single string.
fn extract_coverage_gap_bullets(text: &str) -> Vec<String> {
    let mut in_section = false;
    let mut current: Option<String> = None;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            if in_section {
                if let Some(b) = current.take() {
                    out.push(b);
                }
                break;
            }
            let lower = rest.to_ascii_lowercase();
            if lower.starts_with("coverage gaps") {
                in_section = true;
            }
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ") {
            if let Some(b) = current.take() {
                out.push(b);
            }
            current = Some(rest.trim().to_string());
        } else if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(c) = current.as_mut() {
                c.push(' ');
                c.push_str(line.trim());
            }
        } else if line.trim().is_empty() {
            // paragraph break — bullet continues only with indented lines
        } else if let Some(b) = current.take() {
            out.push(b);
        }
    }
    if let Some(b) = current {
        out.push(b);
    }
    out
}

/// Split `a → b` into `(a, b)`, trimming both halves. Supports both
/// the literal `→` character and the `->` ASCII fallback.
fn split_arrow(line: &str) -> Option<(String, String)> {
    if let Some(idx) = line.find('→') {
        let (left, right) = line.split_at(idx);
        let right = right.trim_start_matches('→');
        return Some((strip_backticks(left), strip_backticks(right)));
    }
    if let Some(idx) = line.find("->") {
        let (left, right) = line.split_at(idx);
        let right = &right[2..];
        return Some((strip_backticks(left), strip_backticks(right)));
    }
    None
}

fn strip_backticks(s: &str) -> String {
    // Cut off the trailing commentary (" — blah blah") that most
    // bullets attach after the tool name, keeping the response
    // compact. Em-dash first, then double ASCII hyphen fallback.
    let trimmed = s.trim();
    let head = trimmed
        .split_once(" — ")
        .map(|(h, _)| h)
        .or_else(|| trimmed.split_once(" -- ").map(|(h, _)| h))
        .unwrap_or(trimmed);
    head.trim().trim_matches('`').trim().to_string()
}

/// Bullets of the form `… — already covered by X` or `… — agent error`
/// are not gaps; they document behaviour the suite already supports
/// or mis-attribution.
fn is_non_gap_annotation(proposed: &str) -> bool {
    let lower = proposed.to_ascii_lowercase();
    lower.contains("already covered") || lower.contains("agent error")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn extracts_unicode_arrow_bullets() {
        let body = "\
# Title

## Coverage gaps

- list files in a directory → `aide_project_ls(prefix, glob?)` — explanation
- free-text grep across repo → `aide_project_grep(pattern)` — note
- symbol search → already covered by lsp_workspace_symbols

## Other section
- not a gap
";
        let dir = TempDir::new().unwrap();
        write(dir.path(), "001.md", body);
        let report = aggregate_coverage_gaps(dir.path()).unwrap();
        assert_eq!(report.runs_scanned, 1);
        assert_eq!(report.gaps.len(), 2);
        let caps: Vec<_> = report.gaps.iter().map(|g| g.capability.as_str()).collect();
        assert!(caps.contains(&"list files in a directory"));
        assert!(caps.contains(&"free-text grep across repo"));
    }

    #[test]
    fn aggregates_and_ranks_across_runs() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "001.md",
            "## Coverage gaps\n- foo → `aide_foo` — one\n",
        );
        write(
            dir.path(),
            "002.md",
            "## Coverage gaps\n- foo → `aide_foo` — two\n- bar → `aide_bar` — once\n",
        );
        let report = aggregate_coverage_gaps(dir.path()).unwrap();
        assert_eq!(report.runs_scanned, 2);
        assert_eq!(report.gaps.len(), 2);
        assert_eq!(report.gaps[0].capability, "foo");
        assert_eq!(report.gaps[0].occurrences, 2);
        assert_eq!(report.gaps[0].run_files.len(), 2);
        assert_eq!(report.gaps[1].capability, "bar");
        assert_eq!(report.gaps[1].occurrences, 1);
    }

    #[test]
    fn empty_runs_dir_returns_zero_report() {
        let dir = TempDir::new().unwrap();
        let report = aggregate_coverage_gaps(dir.path()).unwrap();
        assert_eq!(report.runs_scanned, 0);
        assert!(report.gaps.is_empty());
    }

    #[test]
    fn ascii_arrow_fallback_parses() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "003.md",
            "## Coverage gaps\n- something -> `aide_something` — note\n",
        );
        let report = aggregate_coverage_gaps(dir.path()).unwrap();
        assert_eq!(report.gaps.len(), 1);
        assert_eq!(report.gaps[0].capability, "something");
    }
}
