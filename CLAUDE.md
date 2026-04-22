# CLAUDE.md

Instructions for AI agents working in this repository. Persistent conventions
only — for the current roadmap position, see [STATUS.md](STATUS.md); for the
user-facing overview, see [README.md](README.md).

## Pre-commit checks

Every code change must pass, in this order, before committing:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If any step fails, fix the root cause — do not suppress with `#[allow(...)]`
unless there is a real reason (see below).

## Lint gotchas

- `clippy::pedantic` is enabled workspace-wide in the root `Cargo.toml`.
- rmcp `#[tool]` methods must take `&self` even when they do not use it.
  When you add a stateless tool, annotate it:

  ```rust
  #[tool(description = "...")]
  #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
  fn my_tool(&self, ...) -> String { ... }
  ```

- `AideServer::tool_router` field is read by macro-generated code clippy does
  not see. Keep the existing `#[allow(dead_code, reason = "...")]` on it.
- When a suppression is genuinely needed, always use the `reason = "..."`
  form, not a bare `#[allow(...)]`.

## Architectural invariants

1. **LSP lives on the working tree, SCIP lives on commits.** Tools that
   operate on live (possibly dirty) files route through the LSP pool. Tools
   that operate on a stable snapshot route through SCIP keyed by commit SHA.
   Never build SCIP from dirty state, never ask the LSP about a historical
   commit.
2. **Never write to `$HOME` directly.** The project uses `~/.aide/` for all
   persisted state; `AidePaths::from_home()` handles this. Tests and sandboxed
   invocations override the root with `AIDE_HOME`, not by clobbering `$HOME`
   (rustup and cargo still need it).
3. **Each new language goes through the `LanguagePlugin` trait.** Add a
   module under `crates/aide-lang/src/languages/`, implement the trait,
   register it in `Registry::builtin()`. Do not special-case languages in
   the MCP server crate.
4. **Tool binaries are always pinned.** Any new `ToolSpec` must pin a tag
   (date or version). Never use `latest` — reproducibility matters.
5. **Separate processes talk over unix sockets.** When `aide-indexer` lands,
   the server talks to it via `~/.aide/sock/*.sock`, not shared memory or
   TCP. Daemon state outlives individual MCP sessions.

## Git workflow

- Work on `master`. Auto-commit and push once `fmt + clippy + test` are green
  — the user treats a passing green bar as the signal that changes are ready.
- Commit messages describe **why**, not what (the diff shows what). Follow
  the style of existing history: imperative mood, first line ≤72 chars,
  body wrapping at ~72.
- Every commit ends with:

  ```
  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  ```

- Never `--force`, never `--no-verify`. If a hook fails, fix the underlying
  issue and create a new commit.

## Scope discipline

- New features belong to a specific milestone in the roadmap (see STATUS.md).
  If something does not fit the active milestone, either defer it or start
  a new milestone explicitly.
- Prefer depth over breadth. Adding a second language is not a v0.3 task —
  v0.3 is indexer daemon + post-commit hook.
- Do not introduce abstractions speculatively. Three similar lines beat a
  premature trait.
- Do not add error handling, retries, or validation for scenarios that
  cannot happen inside our own process boundary. Validate at system
  boundaries only (MCP inputs, network, disk).

## End-of-turn summary

Keep it terse. State what changed and what's next — one or two sentences.
The diff already shows what; commit messages and STATUS.md already show where.

## Files you may (and may not) create

- `*.md` docs: only on explicit user request. The user owns documentation
  scope.
- `tmp/` is gitignored for scratch files; use it, not `/tmp`, when the
  content is project-related.
- Never commit `.env`, credentials, or large binaries. `target/` is
  gitignored.

## Smoke test cheat-sheet

```bash
SMOKE_HOME=$(mktemp -d)
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"git_status","arguments":{}}}' \
  | AIDE_HOME="$SMOKE_HOME" ./target/debug/aide-mcp 2>/dev/null
```

For LSP tools, first call `project_setup` to seed rust-analyzer, then wait
~30 s on the first hover/definition/references query — rust-analyzer returns
`null` until it has indexed the workspace.
