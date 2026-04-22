# Project status snapshot

Last updated: 2026-04-22.

## Purpose of this file

This file is a hand-off for a Claude instance picking up aide-mcp on a
different host. The previous host carried a persistent memory system at
`~/.claude/projects/-Users-jirka-workspace-aide-mcp/memory/`. That memory is
not available on a new machine, so the essential project context — goals,
architectural decisions, roadmap, and current state — is mirrored below.

If you are reading this on a new host, you may want to write these memory
entries into the local memory system so they survive across future sessions.

## Current state

- **Branch:** `master`, pushed to `git@github.com:vysokyj/aide-mcp.git`.
- **Last commit:** `2884640` — "add git read tools: status, log, diff, blame".
- **Build:** `cargo build --workspace` succeeds; `cargo clippy --workspace
  --all-targets -- -D warnings` and `cargo test --workspace` green.
- **End-to-end verified:** every MCP tool has been exercised over stdio
  against the real rust-analyzer or the real aide-mcp git repo.

## Roadmap progress

| Milestone | Scope | Status |
|-----------|-------|--------|
| v0.1      | bootstrap + `project_detect` | ✅ done |
| v0.1.1    | `project_setup` (downloads rust-analyzer) | ✅ done |
| v0.2      | LSP proxy: hover, definition, references, document/workspace symbols, diagnostics | ✅ done |
| v0.3      | git read tools + indexer daemon + post-commit hook | 🟡 in progress — git tools done, indexer + hook pending |
| v0.4      | SCIP build + query | ⬜ planned |
| v0.5      | exec tools (run/test/install) | ⬜ planned |
| v0.6      | DAP proxy (Rust via codelldb first) | ⬜ planned |

## Workspace layout

```
crates/
  aide-core/      AidePaths (~/.aide/bin/scip/sock/queue/config.toml), AIDE_HOME override
  aide-install/   ToolSpec, GitHub-release downloader, gzip decode, manifest.json
  aide-lang/      LanguagePlugin trait + Registry; first impl: RustPlugin
  aide-lsp/       framing, LspClient, LspPool, ops (hover/def/refs/symbols/diagnostics)
  aide-git/       libgit2-backed status/log/diff/blame
  aide-mcp/       MCP stdio server exposing every tool via rmcp 1.5
```

## Key decisions

1. **SDK** = `rmcp` 1.5 (official Anthropic Rust MCP SDK, Tier 2 stable).
   Alternatives (`turbomcp`, `rust-mcp-sdk`) rejected as pre-1.0.
2. **Transport** = stdio only for now.
3. **First language** = Rust (dogfood). More languages added via
   `LanguagePlugin` trait; each declares its LSP / SCIP / DAP / package
   manager / runner.
4. **Execution model** = tools operate directly on the user's working tree
   (no worktree/container sandbox in v1).
5. **LSP lives on the working tree**, **SCIP lives on commits**. This is
   non-negotiable — see the architecture snapshot below.
6. **Binary cache** = `~/.aide/bin/` globally (not per-project), with
   `manifest.json` tracking installed versions. Idempotent re-installs.
7. **`AIDE_HOME` env var** overrides the root dir; `$HOME` stays untouched
   so rustup/cargo keep working.
8. **Indexer will be a separate daemon** (`aide-indexer`), not an in-server
   task. Unix socket between server and daemon.
9. **Retention** of SCIP indexes: default 1 (latest HEAD only), configurable.

## Tools implemented

| Tool | Behaviour |
|------|-----------|
| `project_detect(path?)` | Report detected languages for project root. |
| `project_setup(path?)` | Install LSP/SCIP/DAP binaries for detected languages; idempotent. |
| `lsp_hover(file, line, column, root?)` | LSP hover text, or null. |
| `lsp_definition(file, line, column, root?)` | Locations a symbol is defined at. |
| `lsp_references(file, line, column, include_declaration?, root?)` | All call sites. |
| `lsp_document_symbols(file, root?)` | Hierarchical outline of one file. |
| `lsp_workspace_symbols(query, root?)` | Fuzzy symbol search across project. |
| `lsp_diagnostics(file, root?)` | Errors/warnings after a short settle. |
| `git_status(path?)` | Branch, upstream divergence, per-file state. |
| `git_log(path?, limit=20)` | Recent commits from HEAD. |
| `git_diff(path?, mode?, pathspec?)` | Unified diff with stats. |
| `git_blame(path?, file)` | Per-line authorship. |

Modes for `git_diff`: `"head-to-worktree"` (default), `"index-to-worktree"`,
`"head-to-index"`.

## Known issues / caveats

- **First LSP hover often returns `null`** on a fresh workspace because
  rust-analyzer is still indexing. Re-query after a short delay — the LSP
  client is cached per-workspace, so the server stays warm.
- **`lsp_diagnostics`** sleeps 500 ms before returning to let diagnostics
  accumulate. On a big project you may want to extend this.
- **`ToolRouter` dead-code warning** on `AideServer::tool_router` is
  suppressed with `#[allow(dead_code, reason = …)]`; the field is read
  through macro-generated code, which clippy does not see.
- **Windows** is not a supported platform (only aarch64/x86_64 macOS + Linux).

## What to build next

The obvious next step is to finish **v0.3**:

1. New crate `aide-proto` with IPC message types.
2. New binary crate `aide-indexer` — long-lived tokio daemon listening on a
   unix socket at `~/.aide/sock/indexer.sock`.
3. Extend `project_setup` to auto-install `.git/hooks/post-commit` that
   enqueues the new SHA into the daemon. Fall back to a FS watcher on
   `.git/HEAD` when hooks are undesired.
4. From `aide-mcp`, add a client that queries the daemon for
   "is commit X indexed?" and surfaces the answer as MCP tools like
   `work_last_known_state` and `index_status`.

The actual **SCIP build** (downloading `scip-rust`, running it, storing
bytes under `~/.aide/scip/<repo-id>/<sha>.scip`) belongs to v0.4 — split it
out so the daemon scaffolding can land first.

## Build & test

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo fmt --all --check
```

## Smoke-test recipe

```bash
SMOKE_HOME=$(mktemp -d)
# 1. seed rust-analyzer
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"project_setup","arguments":{"path":"'$PWD'"}}}' \
  | AIDE_HOME="$SMOKE_HOME" ./target/debug/aide-mcp
```

For LSP tools, give rust-analyzer ~30 s to index before the second query.

---

## Memory snapshot (imported from previous host)

The three entries below are a verbatim copy of the memory files from the
previous Claude host. They are the canonical source of project context.

### `project_vision` (type: project)

> aide-mcp is a clean MCP server in Rust that gives AI agents real IDE
> capabilities: GIT operations, LSP-based code intelligence, SCIP indexing,
> debugging (DAP), and project lifecycle tools (install, run, test).
>
> **Why:** Agents today work with grep/read primitives and lose the structured
> code knowledge a real IDE provides. Goal is to close that gap via MCP.
>
> **How to apply:**
> - Protocol: pure MCP server (no custom binary wrapper).
> - Language support is incremental — each supported language bundles knowledge
>   of which LSP to download, how to install packages, how to run/test the
>   project.
> - Execution model: tools operate directly against the user's project working
>   tree (no worktree/container sandbox in v1).
> - Scope discipline: prefer deep, correct support for a few languages over
>   broad shallow coverage. First language = Rust (dogfood).
> - Roadmap: v0.1 bootstrap + `project.detect` · v0.2 LSP proxy · v0.3 git +
>   indexer daemon · v0.4 SCIP build/query · v0.5 exec (run/test/install) ·
>   v0.6 DAP proxy (Rust via lldb first).
> - DAP (debug adapter) is a planned first-class capability, not optional —
>   scoped last because protocol is large and runtime-bug value is narrower
>   than LSP/SCIP.
> - Chosen SDK: `rmcp` 1.5.0 (official Anthropic Rust MCP SDK, Tier 2 stable).

### `architecture_git_scip` (type: project)

> aide-mcp is tightly coupled to git. The commit boundary is the canonical
> checkpoint:
>
> - **SCIP index is built after each commit**, not on dirty working tree.
> - SCIP index is **keyed by commit SHA** — cached per commit, never
>   invalidated mid-edit.
> - Agents query "the last stable state of completed work" = HEAD commit's
>   SCIP.
> - Working-tree changes are visible via explicit git diff tools, not via
>   re-indexing.
> - Likely implementation: post-commit hook (or async trigger on commit
>   detection) feeds the indexer.
>
> **Why:** An agent needs a stable semantic snapshot to reason over.
> Reindexing on every keystroke is expensive and noisy; a committed snapshot
> is what the developer considers "done." It also mirrors how humans think —
> the last commit is the meaningful unit of completed work.
>
> **How to apply:**
> - Design LSP tools to operate on live working tree (real-time, dirty OK).
> - Design SCIP tools to operate on commit SHAs (default HEAD), with cache
>   keyed by SHA.
> - Any tool answering "what is the current codebase structure" should
>   distinguish: committed (SCIP) vs uncommitted (git diff + LSP on working
>   tree).
> - The indexer runs as a background job triggered by commit — tools should
>   not block on indexing.
> - Never build SCIP from dirty state.

### `architecture_components` (type: project)

> aide-mcp is split into separate processes with clear responsibilities:
>
> **Processes:**
> - `aide-mcp` — MCP server (stdio), serves tools to the AI agent.
> - `aide-indexer` — long-running daemon that builds SCIP indexes
>   post-commit. Separate process so the server can restart without losing
>   indexing progress, and so indexing survives agent session boundaries.
> - Communication: unix domain socket (likely `~/.aide/sock/indexer.sock`).
>
> **Git hook integration:**
> - `project.setup` tool auto-installs a thin `post-commit` hook that
>   enqueues the new SHA to the indexer daemon.
> - Fallback for users who can't/won't install hooks: filesystem watcher on
>   `.git/HEAD` or `.git/refs/` detects commits.
> - Hook body stays minimal — just pushes SHA to the daemon's socket/queue.
>
> **SCIP cache:**
> - Location: `~/.aide/scip/<repo-id>/<sha>.scip` (+ metadata JSON).
> - Retention: configurable; default = keep only 1 (latest HEAD). This avoids
>   unbounded growth for large repos. Users who want historical indexes bump
>   the config.
>
> **Why:**
> - Daemon split: resilience (server crash doesn't lose index work), clean
>   lifecycle (indexing is long-running background work, not tied to an agent
>   session).
> - Hook + watcher fallback: covers both hook-friendly and hook-averse setups.
> - Default-1 retention: SCIP for a real repo can be large; storing every
>   commit is wasteful for 99% of queries which hit HEAD.
>
> **How to apply:**
> - Tools that need SCIP check cache for requested SHA; if miss, query indexer
>   daemon for status.
> - Tools never block on indexing — return `IndexNotReady { sha, eta }` when
>   cache misses.
> - Retention policy lives in `~/.aide/config.toml` (per-user) with per-repo
>   override.
