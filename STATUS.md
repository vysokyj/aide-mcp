# Project status snapshot

Last updated: 2026-04-26 (v0.22).

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
| v0.4      | SCIP build + query | ✅ done |
| v0.5      | exec tools (run/test/install) | ✅ done |
| v0.6      | DAP proxy (Rust via codelldb first) | ✅ done |
| v0.7      | project-scoped search primitives (`project_ls`, `project_grep`) to make `ls`/`find`/`grep` via Bash unnecessary | ✅ done — `project_ls(_at)`, `project_grep(_at)`, SCIP symbol annotation on grep hits |
| v0.8      | Structured compiler/test feedback: cargo JSON diagnostics parsed into `ExecResult.diagnostics`, each tagged with enclosing SCIP symbol | ✅ done |
| v0.9      | Semantic navigation aggregates: `task_context`, `project_map`, `scip_callers` | ✅ done |
| v0.10     | Test discovery via SCIP: `tests_for_symbol`, `tests_for_changed_files` with plugin-aware `is_test_symbol` heuristic | ✅ done |
| v0.11     | Impact analysis: `impact_of_change` (callers classified test/bin/lib/example/bench), `public_api_diff(sha1, sha2)` | ✅ done |
| v0.12     | Macro / generated code visibility: `lsp_expand_macro` via rust-analyzer's `expandMacro` extension | ✅ done — `cargo expand` fallback deferred |
| v0.13     | Write-side tooling: `lsp_rename_symbol` with LSP-backed cross-file rename | ✅ done |
| v0.13.1   | `lsp_list_code_actions` + `lsp_apply_code_action` (point or range, select by kind or title substring); `apply_workspace_edit` extended to handle `document_changes` | ✅ done |
| v0.13.2   | `safe_edit` — apply unique `old_string → new_string`, snapshot LSP diagnostics before/after, return the classified delta | ✅ done |
| v0.14     | Dogfood → roadmap loop: `dogfood_coverage_gaps` aggregates run records into a ranked report | ✅ done — CI integration deferred |
| v0.15     | Second-language support: Node.js / TypeScript plugin (`typescript-language-server` + `typescript` + `scip-typescript` auto-install via npm tarballs) | ✅ done — DAP (`js-debug`) deferred |
| v0.16     | Third-language support: Python plugin (`pyright` + `scip-python` auto-install via npm tarballs; `python3 -m {pip,pytest}` for package / tests) | ✅ done — DAP (`debugpy`) deferred |
| v0.17     | Fourth-language support: Go plugin (`scip-go` auto-install from GitHub releases; `gopls` system-install, matching scip-java posture). Indexer worker now sets `current_dir(workdir)` so scip-go and any future cwd-sensitive indexer work unchanged. | ✅ done — DAP (`delve`) + gopls auto-install deferred |
| v0.18     | Fifth-language support: C / C++ plugin (`clangd` + `scip-clang` auto-install from GitHub releases; `codelldb` DAP reused from the Rust plugin — one binary, two languages). Detects `CMakeLists.txt`, `compile_commands.json`, `meson.build`, or `.clangd`. | ✅ done — requires user-supplied `compile_commands.json`; Intel-Mac / Linux arm64 scip-clang fall back to system install |
| v0.19     | GitHub integration (piggyback-auth): token-waterfall (`$GITHUB_TOKEN` → `gh auth token` subprocess → `~/.aide/auth/github.token`), four MCP tools (`gh_auth_status`, `gh_issue_create`, `gh_issue_list`, `gh_ux_gotcha`). `gh_ux_gotcha` is **policy-as-code** — it hardcodes the `ux-gotcha` label and the Repro / Why it bites / Suggested fix body template from CLAUDE.md, so the dogfood-friction reporting loop can't drift. New `aide-github` crate follows the existing per-capability split; `reqwest` is already a workspace dep. | ✅ done — native OAuth device flow (variant C) deferred pending OAuth App ownership decision (personal vs. future org / company entity) |
| v0.19.1   | Issue read + reply + close: `gh_issue_view(number)` (returns `{issue, comments}` with full body + state_reason + up to 100 comments), `gh_issue_comment(number, body)`, `gh_issue_close(number, reason?)` with `completed` / `not_planned` reason parse. Closes the dogfood feedback loop — previously aide could file and list issues, but not follow up, dedup, or mark fixed. `Issue` struct gains `body` + `state_reason`; new `Comment` + `IssueUpdate` + `CloseReason` types. | ✅ done |
| v0.20     | Job management for aide-spawned processes: every `run_project` / `run_tests` / `install_package` gets an aide-assigned `job_id` and is tracked in an in-process `Registry` for the spawn's duration. Three new MCP tools — `job_list`, `job_info(id)`, `job_kill(id, signal?)` — operate exclusively on those registered jobs. Signalling uses `nix::sys::signal::kill` (added workspace dep) and accepts `term` (default) / `kill` / `int` / `hup` / `quit` plus `SIG`-prefixed aliases and POSIX numbers. **Scope gate:** tools take `job_id`, never raw PID, so there is no API path for an agent to signal a process aide did not spawn. | ✅ done |
| v0.20.1   | Read-only system process listing: `process_list(name_filter?, limit?)` over `sysinfo` 0.33, scoped to the current user's processes, with a case-insensitive name-substring filter and a result cap (default 200). Returns `{pid, name, exe, cmd, cwd, started_at_unix, memory_bytes, cpu_percent, status}` per process, sorted by PID ascending. Complements v0.20 by answering "which PID is the running aide-mcp?" without a Bash shell-out to `ps`. Generic `process_kill(pid)` remains explicitly rejected — Bash escape hatch stays for that. | ✅ done |
| v0.21     | PR workflow — four MCP tools (`gh_pr_create`, `gh_pr_view`, `gh_pr_list`, `gh_pr_checks`). `gh_pr_create` auto-detects `head` from the current git branch (new `aide_git::current_branch` helper) and `base` from the repo's configured default branch (new `get_repo` client method). `gh_pr_checks` bundles PR → head SHA → check-runs in one tool call. New types in `aide-github`: `PullRequest` + `Branch` + `PullRequestCreate` + `PullRequestListFilter` + `Repo` + `CheckRun` + `CheckRunsResponse`. Reuses `IssueState` for state filtering so the same "open/closed/all" parsing applies. | ✅ done — PR review comments, merge / reopen actions, fine-grained commits-in-PR view deferred to later if dogfood surfaces need |
| v0.22     | SCIP↔LSP enrichment parity: `lsp_diagnostics`, `lsp_references`, `lsp_definition`, `safe_edit`, and `task_context` diagnostics now carry `enclosing_symbol` from the latest Ready SCIP index — same trick that already enriched `project_grep` and `run_*` diagnostics. Closes the inconsistency where some location/diagnostic results reached the agent semantically tagged and others didn't. | ✅ done |

## Workspace layout

```
crates/
  aide-core/      AidePaths (~/.aide/bin/scip/sock/queue/logs/config.toml),
                  AIDE_HOME override, Config (TOML) for scip/exec/dap
                  tunables
  aide-install/   ToolSpec, GitHub-release + DirectUrl downloader,
                  gzip/tar.gz/zip extract, post-extract `custom_install`
                  hook, manifest.json
  aide-lang/      LanguagePlugin trait + Registry; built-ins: RustPlugin,
                  JavaMavenPlugin, JavaGradlePlugin, NodePlugin, PythonPlugin,
                  GoPlugin, CppPlugin
  aide-lsp/       LspClient (spawn takes plugin-supplied args), LspPool,
                  ops (hover/def/refs/symbols/diagnostics)
  aide-dap/       DapClient speaking Debug Adapter Protocol over stdio
                  (initialize, launch, setBreakpoints, continue,
                  stackTrace, scopes, variables, evaluate, disconnect)
  aide-git/       libgit2-backed status/log/diff/blame + export_commit + resolve_head
  aide-proto/     Shared primitives: Content-Length framing + indexer
                  schema (IndexState, CommitInfo)
  aide-scip/      scip protobuf loader + query helpers (documents/symbols/refs)
  aide-search/    project-scoped ls + grep: libgit2 index walk (Scope::Tracked),
                  ripgrep `ignore` walker (Scope::All), git2 status (Dirty /
                  Staged), grep-regex + grep-searcher engine with smart-case
                  and binary skip
  aide-mcp/       MCP stdio server exposing every tool via rmcp 1.5. Owns the
                  in-process SCIP indexer (src/indexer/: state + worker) and
                  the shared exec runner (src/exec.rs).
```

## Key decisions

1. **SDK** = `rmcp` 1.5 (official Anthropic Rust MCP SDK, Tier 2 stable).
2. **Transport** = stdio only.
3. **Languages** = Rust (dogfood) + Java (Maven and Gradle) + Node/TS
   + Python + Go + C/C++. Added via the `LanguagePlugin` trait; each
   declares its LSP / SCIP / DAP / package manager / runner, plus the
   full command line for its SCIP indexer (`scip_args`) and optional
   LSP launch flags (`lsp_spawn_args`). Rust auto-installs
   rust-analyzer + codelldb. Java auto-installs JDT-LS from the
   Eclipse snapshot tarball via a generated wrapper script. scip-java
   still expects a system install. Node/TS auto-installs
   `typescript-language-server`, `typescript`, and `scip-typescript`
   via pinned npm registry tarballs with generated shell wrappers
   (node runtime is a system prerequisite). Python auto-installs
   `pyright` and `scip-python` through the same npm-tarball path
   (node + python3 are system prerequisites). Go auto-installs
   `scip-go` from GitHub releases; `gopls` and the Go toolchain
   itself are system prerequisites — matching the scip-java posture.
   C/C++ auto-installs `clangd` (zip) + `scip-clang` (raw binary)
   from GitHub releases and reuses `codelldb` from the Rust plugin
   as its DAP adapter (one download, two languages).
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
11. **Retention** of SCIP indexes: default 1 (latest HEAD only by
    `enqueued_at_unix`). Enforced inside `Store::mark_ready` — older
    Ready commits are evicted from state and their `.scip` files are
    deleted by the worker. Configurable retention count will land when
    `~/.aide/config.toml` exists.
12. **Exec logs**: `run_project` / `run_tests` / `install_package` tee
    their full stdout/stderr to
    `~/.aide/logs/<ts>-<bin>.{stdout,stderr}.log`. The JSON response
    still caps each stream at 1 MB in memory; the log files hold the
    complete output for post-mortem when `*_truncated` is true.
13. **Config is hot-reloaded**: the MCP server polls
    `~/.aide/config.toml` every 5 s and swaps the live values in
    place. Editing the file does not require restarting MCP.
14. **Exec tools emit MCP progress notifications** when the client
    attaches a `progressToken`: one heartbeat per second for the
    duration of a `run_project` / `run_tests` / `install_package`
    call. Clients can pair this with `read_exec_log` for
    tail-and-progress UX.
15. **Multi-file tool installs** extend `aide-install` via
    `ArchiveFormat::TarGz` / `Zip` (extract under `~/.aide/bin/<name>-
    <version>/` and symlink the entry) plus an optional
    `custom_install: fn(&Path, &Path) -> Result<(), InstallError>`
    hook that replaces the default symlink step. Used by JDT-LS to
    generate a `java -jar …` wrapper script at install time.

## Tools implemented

| Tool | Behaviour |
|------|-----------|
| `project_detect(path?)` | Report detected languages for project root. |
| `project_ls(path?, scope?, glob?, max_results?, include_hidden?)` | Enumerate files under the project root. Scope = tracked (default, libgit2 index) / all (gitignore-aware walk) / dirty / staged. Optional glob filter over the relative path. |
| `project_grep(pattern, path?, scope?, glob?, case_sensitive?, before_context?, after_context?, max_results?, max_results_per_file?, include_hidden?)` | Regex search powered by grep-regex + grep-searcher. Smart-case by default, binary files skipped, per-file and total result caps, optional context lines tagged match/before/after. |
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
| `scip_documents(path?, sha?)` | Paths covered by the SCIP index for a commit. Default = last Ready. |
| `scip_symbols(path?, query, sha?)` | Fuzzy-search SCIP symbols by display_name or symbol id. |
| `scip_references(path?, symbol, sha?)` | Every occurrence of a SCIP symbol id (with `is_definition`). |
| `run_project(path?, extra_args?, timeout_secs?)` | Invoke plugin.runner (e.g. `cargo run`); capture stdout/stderr/exit. |
| `run_tests(path?, filter?, extra_args?, timeout_secs?)` | Invoke plugin.test_runner (e.g. `cargo test [filter]`). |
| `install_package(path?, packages, timeout_secs?)` | Invoke plugin.package_manager (e.g. `cargo add <pkg>`). |
| `read_exec_log(path, offset?, max_bytes?)` | Read a chunk of an exec log file; poll to stream output. |
| `dap_launch(path?, program, args?, stop_on_entry?, env?, session?)` | Start a DAP session via plugin.dap (Rust = codelldb). Full initialize → launch → configurationDone handshake; returns `{ session, stopped }`. |
| `dap_set_breakpoints(source, lines, session?)` | Set line breakpoints on `source` for the named session. |
| `dap_continue(thread_id?, session?)` | Resume the paused thread and wait for next stop. |
| `dap_stack_trace(thread_id?, session?)` | Current call stack (up to 50 frames). |
| `dap_scopes(frame_id, session?)` | Scopes for a frame (Locals, Registers, …). |
| `dap_variables(variables_reference, session?)` | Read variables for a scope / composite variable. |
| `dap_evaluate(expression, frame_id?, session?)` | Evaluate an expression in the debuggee. |
| `dap_terminate(session?)` | Disconnect the named session. |
| `dap_step_over(thread_id?, session?)` | Step to the next source line in the same frame. |
| `dap_step_in(thread_id?, session?)` | Enter a call at the current line. |
| `dap_step_out(thread_id?, session?)` | Run until the current frame returns. |
| `dap_pause(thread_id, session?)` | Suspend a running thread. |

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

Core roadmap (v0.1 through v0.6) plus two polish rounds are complete.
v0.7 landed with `project_ls(_at)`, `project_grep(_at)`, and SCIP
symbol annotation on grep hits. The next milestones focus on what the
dogfood benchmark keeps revealing: agents still burn roundtrips on
unstructured compiler output, on reassembling context from many small
queries, and on text-level editing when the semantic layer is right
there.

### v0.8 — Structured compiler / test feedback

Agents today consume wall-of-text from `cargo test`, `cargo check`,
`cargo clippy` and re-derive "what does this error mean, where does
it come from, which function am I breaking." Cargo already speaks
JSON (`--message-format=json`); parsing it on the aide side means
each diagnostic surfaces as:

```json
{
  "level": "error",
  "code": "E0382",
  "message": "borrow of moved value: `x`",
  "file": "src/foo.rs",
  "line": 42,
  "enclosing_symbol": "Foo::process",
  "spans": [ ... ],
  "suggested_fix": "..."
}
```

The enclosing-symbol trick is identical to the one `project_grep`
now uses — resolve the last Ready SCIP, feed each diagnostic line
through `enclosing_definition`. `lsp_diagnostics` gets the same
annotation for free.

Shape: add `diagnostics: Vec<StructuredDiagnostic>` alongside the
existing `stdout`/`stderr` on `ExecResult`. Plugins get a
`parse_diagnostics(stdout) -> Vec<Diagnostic>` hook with a default
empty impl; the Rust plugin flips `cargo` into JSON mode via extra
args.

### v0.9 — Semantic navigation aggregates

Individual LSP/SCIP tools are sharp but agents still open sessions
with a cascade of `lsp_document_symbols` + `git_log` + `git_blame`
+ `lsp_diagnostics` just to orient. One aggregate call beats five:

- `task_context(file)` — document symbols, recent blame (author +
  commit message), current diagnostics, HEAD→worktree diff for this
  file, enclosing crate/module. One MCP round-trip.
- `project_map(path?)` — public API surface digest from the last
  Ready SCIP: crates, modules, pub traits/types, entry points.
  Replaces the "grep `pub fn`" reflex.
- `scip_callers(symbol)` / `scip_callees(symbol)` — thin wrappers
  over `scip_references` that split definition from use and group
  by file. What agents actually want.

### v0.10 — Test discovery via SCIP

"I edited foo::bar, which tests cover it?" is the most expensive
question to answer today (read every `#[test]`, guess from names).
SCIP already has the call graph:

- `tests_for_symbol(symbol)` — any test function that transitively
  references `symbol` via SCIP edges.
- `tests_for_changed_files(since?)` — union of the above for every
  symbol defined in dirty/staged files (or diffed since a ref).
- `run_tests` gains `derive_filter: bool` that feeds this directly
  into `cargo test <filter>`.

### v0.11 — Impact analysis

Before a risky edit, answer "how wide is the blast radius":

- `impact_of_change(symbol)` — callers classified as test vs lib
  vs bin, with enclosing symbol for each call site.
- `public_api_diff(sha1, sha2)` — structured diff of the pub surface
  between two commits (added / removed / signature-changed). Much
  sharper than `git diff | grep pub`.

### v0.12 — Macro / generated-code visibility

Macro-heavy crates (serde, clap, sqlx, tokio::select) are where
agents flail the most because the apparent source is not what the
compiler sees. Two cheap wins:

- `lsp_expand_macro(file, line, col)` via rust-analyzer's "Expand
  macro recursively" code action.
- `run_cargo_expand(path, target)` subprocess fallback when the
  code action isn't available or for whole-module expansion.

Same idea applies to Lombok in Java later.

### v0.13 — Write-side tooling

aide today reads semantically but agents still edit by `Edit`/regex.
This is where the biggest correctness wins live:

- `edit_by_symbol(symbol_id, new_body)` — LSP workspace edits keyed
  by SCIP symbol id. No scope guessing.
- `lsp_rename_symbol(file, line, col, new_name)` — proper cross-file
  rename via LSP `textDocument/rename`.
- `apply_code_action(file, line, kind)` — invoke LSP code actions
  ("organize imports", "add missing match arm", "fill struct
  fields") by action kind.
- `safe_edit(edits)` — wrap any write with a before/after diagnostic
  diff. Returns "your change added N new errors in M files" so the
  agent can self-correct without re-running the build.

Architectural step: adds `apply` semantics to the server. Needs a
careful conflict-resolution story for concurrent LSP + filesystem
writes.

### v0.14 — Dogfood → roadmap loop

The paired-agent benchmark already emits a `Coverage gaps` section
per run ("this Bash call had no aide equivalent"). Today those
gaps live in `dogfood/runs/NNN-*.md` and have to be read by hand.
Aggregate across runs: `dogfood_gap_report()` surfacing the
most-common missing tools, ordered by frequency and recency. Close
the loop between "what agents actually need" and "what aide
ships." Optional: wire it to CI so every merge re-benchmarks.

### Deferred from the v0.8–v0.14 batch

Four items were scoped out of their parent milestones to keep each
commit clean. Each one has a concrete blocker — none is "we forgot."

- **`run_cargo_expand` (from v0.12)** — `cargo expand` would cover
  whole-module expansion that the LSP-level `lsp_expand_macro`
  cannot (LSP expands one invocation at a time). Blocker: the tool
  isn't shipped with cargo; users would need `cargo install
  cargo-expand`. Not zero-friction enough to land before a real
  request surfaces.
  *Proposed move:* park until a dogfood run surfaces "needs
  whole-module expansion." Implement then as a 30-line wrapper
  around `exec::run("cargo", ["expand", …])` with an early-return
  pointing at the install command when the binary is missing.

- **Pull-diagnostics in `safe_edit` (v0.13.2 refinement)** —
  `safe_edit` currently uses the published-diagnostics path with a
  fixed `settle_ms` wait, which is "best-effort" on cold or large
  workspaces. The LSP 3.17+ pull-model `textDocument/diagnostic`
  request is the way to get synchronous "what do you think now?"
  answers. *Proposed move:* deferred until a real run hits the
  fixed-wait limit. When it does: probe `diagnosticProvider` at
  `initialize`, use the pull path when available, keep the
  published-stream path as universal fallback, promote the
  `confidence` field from `"best_effort"` to `"synchronous"` when
  pull succeeded.

- **Dogfood CI integration (from v0.14)** — the aggregator
  exists; what's missing is an automated GitHub Action that runs
  the paired benchmark on each merge and turns the top-ranked
  coverage gaps into tracked issues. Blocker is operational, not
  technical: we need to decide when benchmarks run (every push is
  too noisy, nightly may be cheaper), what to do with duplicate
  issues, and who owns triage.
  *Proposed move:* not before dogfood becomes routine (≥10 runs
  accumulated). Then: one workflow file, nightly schedule, uses
  `dogfood_coverage_gaps` to generate a single "weekly gap
  report" issue that supersedes the previous week's. No
  auto-multiplexing into per-capability issues until we see how
  stable the bullets actually are across runs.

### Proposed next milestone

The v0.8–v0.22 batch plus v0.13.1 and v0.13.2 all shipped. The
remaining deferrals (`run_cargo_expand`, pull-diagnostics refinement,
dogfood CI, `js-debug` DAP for Node, `debugpy` DAP for Python,
`delve` DAP for Go, `gopls` auto-install, C/C++ compile-database
auto-generation) are each gated on evidence that isn't yet in the
repo — a run that demands whole-module macro expansion, a
safe_edit call whose fixed wait genuinely misses reanalysis, enough
dogfood runs to make CI worthwhile, a real project where debugging
is the friction, a second ecosystem (Ruby gems, Haskell cabal) that
needs `<lang> install`-style auto-install and would justify
extending aide-install with a `Source::Custom` variant, or a C/C++
project where asking the user to run `cmake
-DCMAKE_EXPORT_COMPILE_COMMANDS=ON` is the main friction. Rather
than speculate, hold and let the dogfood loop tell us what to pick
up next.

A good next *research* pass (not implementation) is a paired
benchmark over a real multi-file refactor — something that
exercises rename, apply_code_action, and safe_edit end-to-end —
to see whether the write-side trio actually bends the
tool-call-count curve on the aide side. That's the kind of data
that justifies picking one of the remaining deferrals.

### Legacy open items

- **scip-java auto-install** — Sourcegraph distributes via coursier,
  not a standalone tarball. Would need either a coursier bootstrap
  step or wrapping the `scip-java_2.13-*-assembly.jar` release with
  a generated `java -jar` launcher. Not blocking in practice: users
  run `pacman -S scip-java` / `brew install scip-java` today.
- **Multi-session LSP** — one `LspPool` per workspace today is fine;
  multi-client MCP setups might later want explicit pool keys.
- **`install_tool` progress notifications** — currently only the
  `run_*` / `install_package` tools emit MCP progress. Downloading a
  multi-hundred-MB tarball during `project_setup` is silent right now.

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
