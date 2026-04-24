//! Policy-as-code wrapper for filing "UX gotcha" issues.
//!
//! CLAUDE.md § "Reporting UX gotchas" pins three invariants on every
//! such issue:
//!
//! 1. Label `ux-gotcha` must be present.
//! 2. Title must name the implicated tool so the issue is
//!    grep-discoverable later.
//! 3. Body must record that it was filed via this channel so
//!    policy-driven duplicates can be spotted.
//!
//! Rather than hope the agent remembers all three every time, the
//! `gh_ux_gotcha` MCP tool calls [`build`], which enforces them. The
//! agent still owns the actual narrative of the bug — we do not
//! rewrite `body`, only append a provenance footer.

use crate::client::IssueCreate;

const UX_GOTCHA_LABEL: &str = "ux-gotcha";

/// Construct an [`IssueCreate`] with the three invariants baked in.
///
/// - `title`: free-form one-liner. Prefixed with ``` `{tool}` — ``` when
///   the caller has not already prefixed it, so passing an already-
///   prefixed title is idempotent.
/// - `body`: free-form narrative. Passed through unchanged, then a
///   horizontal rule and provenance footer are appended.
/// - `tool`: the aide MCP tool that misbehaved (`project_ls`,
///   `project_grep`, …).
/// - `param`: optional parameter name to narrow provenance when the
///   gotcha is specific to one argument (e.g. `scope` on `project_ls`).
pub fn build(title: &str, body: &str, tool: &str, param: Option<&str>) -> IssueCreate {
    let tool_prefix = format!("`{tool}`");
    let prefixed_title = if title.trim_start().starts_with(&tool_prefix) {
        title.to_string()
    } else {
        format!("{tool_prefix} — {title}")
    };

    let param_bit = match param {
        Some(p) if !p.is_empty() => format!(" param `{p}`"),
        _ => String::new(),
    };
    let body_with_footer = format!(
        "{body}\n\n---\n\n_Filed via `gh_ux_gotcha` from `{tool}`{param_bit} \
         per CLAUDE.md § \"Reporting UX gotchas\"._\n"
    );

    IssueCreate {
        title: prefixed_title,
        body: body_with_footer,
        labels: vec![UX_GOTCHA_LABEL.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_title_with_tool() {
        let issue = build(
            "default scope hides untracked files",
            "repro",
            "project_ls",
            None,
        );
        assert_eq!(
            issue.title,
            "`project_ls` — default scope hides untracked files"
        );
    }

    #[test]
    fn does_not_double_prefix_already_prefixed_title() {
        let issue = build("`project_ls` is confusing", "repro", "project_ls", None);
        assert_eq!(issue.title, "`project_ls` is confusing");
    }

    #[test]
    fn always_adds_ux_gotcha_label() {
        let issue = build("t", "b", "project_grep", None);
        assert_eq!(issue.labels, vec!["ux-gotcha".to_string()]);
    }

    #[test]
    fn footer_mentions_tool_and_policy() {
        let issue = build("t", "agent narrative", "project_ls", Some("scope"));
        assert!(issue.body.starts_with("agent narrative"));
        assert!(issue.body.contains("`gh_ux_gotcha`"));
        assert!(issue.body.contains("`project_ls`"));
        assert!(issue.body.contains("param `scope`"));
        assert!(issue.body.contains("Reporting UX gotchas"));
    }

    #[test]
    fn footer_without_param_has_no_param_clause() {
        let issue = build("t", "b", "project_ls", None);
        assert!(!issue.body.contains("param `"));
    }
}
