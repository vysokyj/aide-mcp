//! Shared line-oriented diagnostic parser for languages whose build
//! tools emit `file:line[:col][: level]: message` text — gcc/clang,
//! javac, and the `go` tool all agree on this shape. Each plugin
//! filters by its own source-file extensions; JSON-speaking tools
//! (cargo) and bracket-style tools (Maven, tsc) keep their own
//! parsers in their plugin modules.

use aide_proto::Diagnostic;

/// Parse `file:line[:col][: level]: message` lines out of `output`.
///
/// A line qualifies when its first `:`-segment ends with one of
/// `exts` (so prose that merely mentions a filename is skipped — the
/// candidate must contain no spaces). `require_level` demands an
/// explicit `error` / `warning` / `note` token after the position:
/// gcc and javac always print one, while `go build` never does —
/// requiring it where available keeps `make`-style noise out.
pub(crate) fn parse_colon_diagnostics(
    output: &str,
    exts: &[&str],
    require_level: bool,
) -> Vec<Diagnostic> {
    output
        .lines()
        .filter_map(|raw| parse_one(raw.trim(), exts, require_level))
        .collect()
}

fn parse_one(line: &str, exts: &[&str], require_level: bool) -> Option<Diagnostic> {
    let (file, rest) = split_file(line, exts)?;
    let rest = rest.strip_prefix(':')?;
    let (line_no, rest) = take_number(rest)?;
    let rest = rest.strip_prefix(':')?;
    let (column, rest) = match take_number(rest) {
        Some((col, after)) => (Some(col), after.strip_prefix(':').unwrap_or(after)),
        None => (None, rest),
    };
    let rest = rest.trim_start();

    let (level, message) = match rest.split_once(':') {
        Some((token, msg)) => match token.trim() {
            "error" | "fatal error" => ("error", msg.trim()),
            "warning" => ("warning", msg.trim()),
            "note" => ("note", msg.trim()),
            _ if require_level => return None,
            _ => ("error", rest),
        },
        None if require_level => return None,
        None => ("error", rest),
    };
    let message = message.trim();
    if message.is_empty() {
        return None;
    }

    Some(Diagnostic {
        level: level.to_string(),
        code: None,
        message: message.to_string(),
        file: Some(file.to_string()),
        line_start: Some(line_no),
        line_end: None,
        column_start: column,
        column_end: None,
        enclosing_symbol: None,
        rendered: Some(line.to_string()),
    })
}

/// Find the source-file prefix of `line`: the earliest occurrence of
/// `<ext>:` whose preceding text contains no spaces. Returns the file
/// path (with any `./` prefix stripped) and the remainder starting at
/// the `:`.
fn split_file<'a>(line: &'a str, exts: &[&str]) -> Option<(&'a str, &'a str)> {
    let mut best: Option<usize> = None;
    for ext in exts {
        let pat = format!("{ext}:");
        if let Some(idx) = line.find(&pat) {
            let end = idx + ext.len();
            if line[..end].contains(' ') {
                continue;
            }
            if best.is_none_or(|b| end < b) {
                best = Some(end);
            }
        }
    }
    let end = best?;
    let file = line[..end].trim_start_matches("./");
    Some((file, &line[end..]))
}

/// Split a leading ASCII number off `s`.
fn take_number(s: &str) -> Option<(u32, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let n = s[..end].parse().ok()?;
    Some((n, &s[end..]))
}

#[cfg(test)]
mod tests {
    use super::parse_colon_diagnostics;

    #[test]
    fn gcc_style_with_level_and_column() {
        let out = "src/main.cpp:12:5: error: 'foo' was not declared in this scope\n\
                   src/util.hpp:3:1: warning: unused variable 'x' [-Wunused]\n\
                   make: *** [all] Error 2\n";
        let diags = parse_colon_diagnostics(out, &[".cpp", ".hpp"], true);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].level, "error");
        assert_eq!(diags[0].file.as_deref(), Some("src/main.cpp"));
        assert_eq!(diags[0].line_start, Some(12));
        assert_eq!(diags[0].column_start, Some(5));
        assert!(diags[0].message.contains("not declared"));
        assert_eq!(diags[1].level, "warning");
    }

    #[test]
    fn go_style_without_level() {
        let out = "./cmd/main.go:7:2: undefined: fmt.Printlnn\n\
                   FAIL\tgithub.com/acme/widget\t0.1s\n";
        let diags = parse_colon_diagnostics(out, &[".go"], false);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file.as_deref(), Some("cmd/main.go"));
        assert_eq!(diags[0].line_start, Some(7));
        assert_eq!(diags[0].message, "undefined: fmt.Printlnn");
    }

    #[test]
    fn javac_style_without_column() {
        let out = "src/main/java/Foo.java:12: error: cannot find symbol\n\
                   1 error\n";
        let diags = parse_colon_diagnostics(out, &[".java"], true);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line_start, Some(12));
        assert_eq!(diags[0].column_start, None);
        assert_eq!(diags[0].message, "cannot find symbol");
    }

    #[test]
    fn prose_mentioning_a_file_is_skipped() {
        let out = "compiling module a/b.go: this is just a status line\n\
                   see docs for x.go: details:1: not a diagnostic\n";
        // First line: text before `.go:` contains spaces after the
        // file candidate... it doesn't — `a/b.go` has no spaces, and
        // the line parses as line-number-missing (status text), so it
        // is dropped by the number check.
        let diags = parse_colon_diagnostics(out, &[".go"], false);
        assert!(diags.is_empty(), "got: {diags:?}");
    }
}
