# Project status snapshot

Last updated: 2026-04-22.

## Purpose of this file

Hand-off for a Claude instance picking up aide-mcp on a different host.
The persistent memory system used by a given Claude session is
machine-local, so the essential project context — goals, architectural
decisions, roadmap, current state — is mirrored here.

## Current state

- **Branch:** `master`, pushed to `git@github.com:vysokyj/aide-mcp.git`.
- **Build:** `cargo build --workspace` succeeds; `cargo clippy
  --workspace --all-targets -- -D warnings` and `cargo test --workspace`
  are green.
- **Verified:** the MCP tool surface has been exercised over stdio; the
  SCIP indexer runs end-to-end against the real rust-analyzer binary.
- **Big architectural change since v0.3:** the separate `aide-indexer`
  daemon + post-commit hook was dropped. The indexer now lives
  in-process inside `aide-mcp` as a background tokio worker. See the
  updated "architecture_components" snapshot at the bottom.

## Roadmap progress

| Milestone | Scope | Status |
|-----------|-------|--------|
| v0.1      | bootstrap + `project_detect` | ✅ done |
| v0.1.1    | `project_setup` (downloads rust-analyzer) | ✅ done |
| v0.2      | LSP proxy: hover, definition, references, document/workspace symbols, diagnostics | ✅ done |
| v0.3      | git read tools + indexer | ✅ done — in-process indexer, no daemon |
| v0.4      | SCIP build + query | 🟡 in progress — build done, query pending |
| v0.5      | exec tools (run/test/install) | ⬜ planned |
| v0.6      | DAP proxy (Rust via codelldb first) | ⬜ planned |

## Workspace layout

```
crates/
  aide-core/      AidePaths (~/.aide/bin/scip/sock/queue/config.toml), AIDE_HOME override
  aide-install/   ToolSpec, GitHub-release downloader, gzip decode, manifest.json
  aide-lang/      LanguagePlugin trait + Registry; first impl: RustPlugin
  aide-lsp/       framing, LspClient, LspPool, ops (hover/def/refs/symbols/diagnostics)
  aide-git/       libgit2-backed status/log/diff/blame + export_commit + resolve_head
  aide-proto/     Shared schema (IndexState, CommitInfo) for indexer tool responses
  aide-mcp/       MCP stdio server exposing every tool via rmcp 1.5. Owns the
                  in-process SCIP indexer (src/indexer/: state + worker).
```

## Key decisions

1. **SDK** = `rmcp` 1.5 (official Anthropic Rust MCP SDK, Tier 2 stable).
2. **Transport** = stdio only.
3. **First language** = Rust (dogfood). More languages added via
   `LanguagePlugin` trait; each declares its LSP / SCIP / DAP / package
   manager / runner, plus the full command line for its SCIP indexer
   (`scip_args`).
4. **Execution model** = MCP tools operate directly against the user's
   working tree. SCIP is built against a commit snapshot exported to a
   TempDir — never against the dirty working tree.
5. **LSP lives on the working tree**, **SCIP lives on commits**. Enforced:
   LSP tools route through `LspPool`; SCIP work calls
   `aide_git::export_commit` before invoking the indexer.
6. **Binary cache** = `~/.aide/bin/` globally, with `manifest.json`
   tracking installed versions. Idempotent re-installs.
7. **`AIDE_HOME` env var** overrides the root dir; `$HOME` stays
   untouched so rustup/cargo keep working.
8. **Indexer is in-process, not a daemon.** One background tokio worker
   per MCP server. State persists at `~/.aide/queue/indexer_state.json`
   and survives MCP restarts; ready `.scip` files remain usable. No
   unix socket, no post-commit hook.
9. **Indexer triggers** are agent-driven: `project_setup` enqueues HEAD,
   each `git_*` tool enqueues HEAD after running (cheap no-op when
   already Ready), and agents can force a refresh via `index_commit`.
10. **SCIP** is produced by `rust-analyzer scip` — the same binary that
    serves LSP — so no second tool to download per language.
11. **Retention** of SCIP indexes: default 1 (latest HEAD only),
    configurable. Not yet implemented — files accumulate under
    `~/.aide/scip/<slug>/` for now.

## Tools implemented

| Tool | Behaviour |
|------|-----------|
| `project_detect(path?)` | Report detected languages for project root. |
| `project_setup(path?)` | Install LSP/SCIP/DAP binaries for detected languages; idempotent. Enqueues HEAD for SCIP indexing. |
| `lsp_hover(file, line, column, root?)` | LSP hover text, or null. |
| `lsp_definition(file, line, column, root?)` | Locations a symbol is defined at. |
| `lsp_references(file, line, column, include_declaration?, root?)` | All call sites. |
| `lsp_document_symbols(file, root?)` | Hierarchical outline of one file. |
| `lsp_workspace_symbols(query, root?)` | Fuzzy symbol search across project. |
| `lsp_diagnostics(file, root?)` | Errors/warnings after a short settle. |
| `git_status(path?)` | Branch, upstream divergence, per-file state. Enqueues HEAD for indexing. |
| `git_log(path?, limit=20)` | Recent commits from HEAD. Enqueues HEAD for indexing. |
| `git_diff(path?, mode?, pathspec?)` | Unified diff with stats. Enqueues HEAD for indexing. |
| `git_blame(path?, file)` | Per-line authorship. |
| `index_commit(path?, sha?)` | Explicitly enqueue a commit (HEAD by default) for SCIP indexing. |
| `index_status(path?, sha?)` | State of a commit in the indexer (Pending/InProgress/Ready/Failed). |
| `work_last_known_state(path?)` | Last commit the indexer knows about for this repo. |

Modes for `git_diff`: `"head-to-worktree"` (default), `"index-to-worktree"`,
`"head-to-index"`.

## Known issues / caveats

- **First LSP hover often returns `null`** on a fresh workspace because
  rust-analyzer is still indexing. Re-query after a short delay — the
  `LspPool` caches the client per workspace, so the server stays warm.
- **`lsp_diagnostics`** sleeps 500 ms before returning to let
  diagnostics accumulate. On a big project you may want to extend this.
- **`ToolRouter` dead-code warning** on `AideServer::tool_router` is
  suppressed with `#[allow(dead_code, reason = …)]`; the field is read
  through macro-generated code, which clippy does not see.
- **Indexer tests need a real filesystem for unix sockets?** No — the
  socket path is gone. All indexer tests now run inside the default
  sandbox.
- **SCIP build on large projects** runs `cargo metadata` under the
  hood (via rust-analyzer), which fetches dependencies. No shared
  target cache across commits yet, so each new commit re-fetches.
- **Windows** is not a supported platform (only aarch64/x86_64 macOS +
  Linux).

## What to build next

Finish **v0.4**:

1. New crate `aide-scip` loading and parsing `.scip` files (protobuf
   via the official `scip` crate).
2. MCP tools `scip_documents`, `scip_symbols`, `scip_references` that
   read `indexer.last_ready(repo)` → resolve the `.scip` path → run
   queries.
3. Optional: retention — when a new commit becomes Ready, prune older
   `.scip` files for that repo to keep only the latest (configurable
   via `~/.aide/config.toml`).

Then start **v0.5** (exec tools): `run`, `test`, `install_package`
invoking the language plugin's `runner` / `test_runner` /
`package_manager` and streaming output back to the agent.

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
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"project_setup","arguments":{"path":"'$PWD'"}}}' \
  | AIDE_HOME="$SMOKE_HOME" ./target/debug/aide-mcp
```

For LSP tools, give rust-analyzer ~30 s to index before the second
query. For SCIP, run `git_log` or `index_commit` to trigger the worker
and then `index_status` to check progress; expect Ready within a
minute on a small Rust project.

---

## Memory snapshot (imported from previous host)

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
>   in-process indexer · v0.4 SCIP build/query · v0.5 exec (run/test/install) ·
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
> - Commit detection is agent-driven: `project_setup` and the `git_*` tools
>   enqueue HEAD for indexing after they run; agents can also call
>   `index_commit` explicitly.
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
> - Before running the SCIP indexer, export the commit's tree to a fresh
>   TempDir via `aide_git::export::export_commit`. Never index the source
>   repo's working tree.

### `architecture_components` (type: project, **updated 2026-04-22**)

> aide-mcp runs as a single process. The earlier plan for a separate
> `aide-indexer` daemon + post-commit hook was scrapped because in
> practice the MCP server is already the only long-lived process during
> agent work, and a daemon adds operational weight (lifecycle, socket,
> hook install, OS-specific autostart) for no extra capability.
>
> **Process:**
> - `aide-mcp` — MCP server over stdio. Owns an `Indexer` service (see
>   `crates/aide-mcp/src/indexer/`) with a persistent `Store` and a
>   background tokio worker task. On startup it loads the state file
>   and re-enqueues anything left Pending / InProgress from the
>   previous session.
>
> **Commit detection (no hook):**
> - `project_setup` enqueues the current HEAD at the end.
> - `git_status`, `git_log`, `git_diff` enqueue HEAD after running —
>   a no-op when the commit is already Ready, so cheap to call
>   repeatedly.
> - Agents can force a refresh via the `index_commit` tool.
>
> **SCIP cache:**
> - Layout: `~/.aide/scip/<slug(abs_repo_root)>/<sha>.scip`.
> - Index state: `~/.aide/queue/indexer_state.json` (atomic writes).
> - Retention: configurable; default = keep only 1 (latest HEAD).
>   **Not yet enforced** — files accumulate.
>
> **How to apply:**
> - Tools that read SCIP take the path from
>   `Indexer::last_ready(repo_root)` (or the explicit `sha` arg) and
>   return an error if no Ready index exists yet.
> - Never block on indexing from an MCP tool — the worker runs
>   independently; callers just peek at state.
> - Retention policy lives in `~/.aide/config.toml` (per-user) with
>   per-repo override.
