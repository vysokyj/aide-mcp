# Tool reference

Practical reference for every MCP tool aide-mcp exposes — one page that
serves both a human looking up "what can I actually call?" and an AI
agent deciding which tool fits the task.

The roadmap / status lives in [STATUS.md](../STATUS.md); this page is
pure lookup.

## How to read this page

The **Replaces** column answers "what Bash/IDE habit does this tool
displace?":

- A command (e.g. `grep -r`) means the aide tool is a drop-in plus
  extra structure or semantics.
- `—` means net-new: there is no Bash equivalent — you literally
  can't get this answer from shell primitives, only from a language
  server or a pre-built index.

Scopes that appear in several tools:

- `tracked` — libgit2 index (fastest; default for `project_ls`/`project_grep`).
- `all` — gitignore-aware working-tree walk.
- `dirty` — files with non-clean status.
- `staged` — files whose index entry differs from HEAD.

## Quick cheat sheet

The Bash habits that have a first-class aide equivalent:

| Bash habit | aide tool | What you gain |
|---|---|---|
| `ls`, `find` | `project_ls` | gitignore-aware; git-scope filters; glob |
| `grep -r`, `rg` | `project_grep` | same engine + each hit annotated with enclosing SCIP symbol |
| `git status` | `git_status` | structured JSON + nudges SCIP indexer |
| `git log` | `git_log` | structured |
| `git diff` | `git_diff` | three modes, structured stats |
| `git blame` | `git_blame` | per-line JSON |
| `cargo run` / `npm start` / `go run` | `run_project` | tee log, structured exit, progress notifications |
| `cargo test` / `pytest` / `go test` | `run_tests` | JSON compiler diagnostics tagged with enclosing symbol |
| `cargo add` / `npm i` / `pip install` | `install_package` | plugin-aware |
| `grep 'pub fn'` / `grep 'class '` | `project_map` | real public-API digest from SCIP |
| `sed` / find-and-replace | `lsp_rename_symbol` | scope-correct cross-file rename |
| plain `Edit` for risky change | `safe_edit` | before/after diagnostic delta |
| `tail -f target/…log` | `read_exec_log` | |
| `gh auth status` | `gh_auth_status` | resolves token via env → `gh` subprocess → file, reports `{source, login, scopes}` |
| `gh issue create` | `gh_issue_create` | detects repo from `origin`, no repo flag needed |
| `gh issue list` | `gh_issue_list` | structured JSON straight from REST |
| ad-hoc `gh issue create --label ux-gotcha …` | `gh_ux_gotcha` | policy-as-code: label + title prefix + provenance footer guaranteed |

---

## Project bootstrap

| Tool | Replaces | What it does |
|---|---|---|
| `project_detect(path?)` | — | Report which supported languages live at the project root. Returns `{id, lsp}` per language. |
| `project_setup(path?)` | `brew install rust-analyzer && … codelldb && …` | Download LSP / SCIP / DAP binaries for every detected language into `~/.aide/bin/`. Idempotent. Enqueues HEAD for SCIP indexing at the end. |

## Search & filesystem

| Tool | Replaces | What it does |
|---|---|---|
| `project_ls(scope?, glob?, max_results?, include_hidden?)` | `ls`, `find` | Enumerate files. Scopes `tracked` (default, libgit2 index) / `all` / `dirty` / `staged`. Glob filter over repo-relative path. Default cap 500. |
| `project_grep(pattern, scope?, glob?, case_sensitive?, before_context?, after_context?, max_results?, max_results_per_file?)` | `grep -r`, `rg` | ripgrep engine (grep-regex + grep-searcher). Smart-case by default, binary skip, per-file and total caps (defaults 50 / 200), optional context lines tagged match/before/after. **SCIP bonus:** each hit line annotated with `symbol` = enclosing definition's display name, when a Ready SCIP exists. |
| `project_ls_at(sha, glob?, max_results?)` | `git ls-tree --name-only <sha>` | Same enumeration but against a specific commit's tree — reads git tree objects directly, no worktree checkout, no TempDir, no filesystem side effects. For auditing historical state. |
| `project_grep_at(sha, pattern, glob?, case_sensitive?, before_context?, after_context?, max_results?, max_results_per_file?)` | `git grep <sha>` | Same search but against a commit's tree. Reads blob bytes directly from libgit2 and runs ripgrep over them. |

## Git read ops

All `git_*` tools also nudge the SCIP indexer — cheap no-op when HEAD is
already Ready, so safe to call repeatedly.

| Tool | Replaces | What it does |
|---|---|---|
| `git_status(path?)` | `git status` | Branch, upstream divergence, per-file working-tree + index state. |
| `git_log(path?, limit=20)` | `git log` | Recent commits. |
| `git_diff(path?, mode?, pathspec?)` | `git diff` | Modes: `head-to-worktree` (default) / `index-to-worktree` / `head-to-index`. Unified diff + stats. |
| `git_blame(path?, file)` | `git blame` | Per-line authorship as JSON. |

## LSP navigation — live working tree

The LSP server (rust-analyzer / JDT-LS / pyright / gopls / clangd /
typescript-language-server) stays warm per workspace in an `LspPool`.
First query after opening a fresh workspace may return `null` — retry in
~30 s.

| Tool | Replaces | What it does |
|---|---|---|
| `lsp_hover(file, line, column, root?)` | — | Hover text, or null. |
| `lsp_definition(file, line, column, root?)` | — | Jump-to-definition. |
| `lsp_references(file, line, column, include_declaration?, root?)` | `grep <name>` | All call sites, semantically scoped. |
| `lsp_document_symbols(file, root?)` | — | Hierarchical outline of one file. |
| `lsp_workspace_symbols(query, root?)` | `grep` for a name | Fuzzy symbol search. Empty query = top-level symbols. |
| `lsp_diagnostics(file, root?)` | `cargo check` on one file | Errors/warnings after a short settle (500 ms). |

## SCIP semantic — committed snapshot

Query against the last Ready SCIP index, keyed by commit SHA. Complements
LSP: LSP is live, SCIP is stable with a full pre-computed call graph. If
`index_status` is not `Ready`, these tools error — `git_status` or
`index_commit` to nudge the worker, then poll.

| Tool | Replaces | What it does |
|---|---|---|
| `scip_documents(sha?)` | — | All paths covered by the SCIP index for a commit. |
| `scip_symbols(query, sha?)` | — | Fuzzy-search SCIP symbols (display_name or symbol id). |
| `scip_references(symbol, sha?)` | — | Every occurrence of a SCIP symbol id, with `is_definition` flag per hit. |
| `scip_callers(symbol, sha?)` | `grep <name>(` | Call sites only (definitions filtered out), grouped by file. |

## Aggregates — one call instead of five

| Tool | Replaces | What it does |
|---|---|---|
| `task_context(file, history_limit?)` | `lsp_document_symbols` + `lsp_diagnostics` + `git_diff` + `git_log` + SCIP top-level | One call to orient around a file. Partial failure (LSP cold, no SCIP Ready) leaves that field null/empty; the rest stays valid. |
| `project_map(kinds?, sha?)` | `grep 'pub fn' / 'class '` | Public-API digest from the last Ready SCIP: per-document top-level symbols (name, kind, definition line). Filter by kinds `[Function, Struct, Enum, Trait, Class, Method, …]`. |

## Test discovery

| Tool | Replaces | What it does |
|---|---|---|
| `tests_for_symbol(symbol, sha?)` | "grep `#[test]` and guess" | Every test function that transitively references `symbol` via SCIP edges. Plugin-aware `is_test_symbol` heuristic (Rust `#[test]`, Java `@Test`, Node `describe/it`, Python `test_*`, Go `Test*`). |
| `tests_for_changed_files(sha?)` | manual mapping dirty→tests | Reads dirty + staged files from `git status`, then returns the union of **(a)** tests defined directly in those files and **(b)** tests that transitively reference any symbol defined in them. Deduplicated, tagged with file and line. Feed display names into `run_tests` as filters. |

## Impact analysis

| Tool | Replaces | What it does |
|---|---|---|
| `impact_of_change(symbol, sha?)` | `grep + classify by hand` | Callers classified as test / bin / lib / example / bench (via the plugin's path heuristic), with enclosing symbol per call site. Each entry gains a `category` field. |
| `public_api_diff(sha1, sha2, kinds?)` | `git diff \| grep pub` | Structured diff of the public surface between two commits. Both SHAs must have a Ready SCIP index (call `index_commit` first if not). Returns `{added, removed}` symbol lists; narrow with `kinds` (e.g. `["Function","Trait"]`) for semver impact. |

## Macro / generated-code visibility

| Tool | Replaces | What it does |
|---|---|---|
| `lsp_expand_macro(file, line, column)` | `cargo expand` (whole crate) | Single-invocation macro expansion via rust-analyzer's `expandMacro` extension. Covers derive / attribute / fn-like macros at that position. **Currently Rust-only** — other languages ignore the request. |

## Write-side tooling

| Tool | Replaces | What it does |
|---|---|---|
| `lsp_rename_symbol(file, line, column, new_name)` | `sed`, Find&Replace | Cross-file rename via LSP `textDocument/rename`. Applies the WorkspaceEdit to disk, keeps LSP buffers in sync. Returns `{files, total_edits}` or null if the symbol at that position isn't renameable. Scope-correct — respects traits and reexports. |
| `lsp_list_code_actions(file, line, column, end_line?, end_column?)` | — | List LSP code actions offered at a point (end collapses to cursor) or range. Each entry: `title`, optional `kind`, `disabled` flag. |
| `lsp_apply_code_action(file, line, column, end_line?, end_column?, kind? \| title?)` | — | Apply one by exact `kind` (e.g. `"source.organizeImports"`) or case-insensitive `title` substring. One of the two is required. Resolves lazy stubs, applies both `changes` and `document_changes` edit shapes, dispatches any attached `workspace/executeCommand`. Returns `{title, kind, applied_edit: {files, total_edits}, ran_command}` or null if nothing matched. |
| `safe_edit(file, old_string, new_string, related_files?, settle_ms?, root?)` | plain `Edit` | Apply unique `old_string → new_string`. Snapshots LSP diagnostics before and after across `file` plus `related_files` (pass caller files for propagation checks). `settle_ms` (default 1500) waits between the edit and the after-snapshot; raise on slow servers. Returns the classified delta — new errors, new warnings, resolved findings, unchanged count — plus `confidence: "best_effort"` (not a replacement for `run_tests` when stakes are high). |

## Exec — run / test / install

All three tee full stdout/stderr to
`~/.aide/logs/<ts>-<bin>.{stdout,stderr}.log`. JSON response caps each
stream at 1 MB; the log file holds the complete output for post-mortem
when `*_truncated` is true. Clients that attach a `progressToken` receive
one heartbeat per second for the duration of the call.

| Tool | Replaces | What it does |
|---|---|---|
| `run_project(path?, extra_args?, timeout_secs?)` | `cargo run` / `npm start` / `python -m …` / `go run` | Invoke the plugin's runner. Structured exit code. |
| `run_tests(path?, filter?, extra_args?, timeout_secs?)` | `cargo test` / `pytest` / `go test` / `mvn test` | Invoke the plugin's test runner. Cargo JSON diagnostics parsed into `diagnostics: [{level, code, message, file, line, enclosing_symbol, suggested_fix, …}]`. |
| `install_package(path?, packages, timeout_secs?)` | `cargo add` / `npm i` / `pip install` / `go get` | Plugin-aware install. |
| `read_exec_log(path, offset?, max_bytes?)` | `tail -f` on the log | Read a chunk; poll to stream. |

## DAP — debugging

Debug Adapter Protocol over stdio. Rust and C/C++ share `codelldb`; other
languages deferred (Node via `js-debug`, Python via `debugpy`, Go via
`delve`).

| Tool | What it does |
|---|---|
| `dap_launch(program, args?, stop_on_entry?, env?, session?)` | `initialize` → `launch` → `configurationDone`. Returns `{session, stopped}`. |
| `dap_set_breakpoints(source, lines, session?)` | Line breakpoints for the named session. |
| `dap_continue(thread_id?, session?)` | Resume and wait for next stop. |
| `dap_pause(thread_id, session?)` | Suspend a running thread. |
| `dap_step_over(thread_id?, session?)` | Step to next line in same frame. |
| `dap_step_in(thread_id?, session?)` | Enter a call at the current line. |
| `dap_step_out(thread_id?, session?)` | Run until current frame returns. |
| `dap_stack_trace(thread_id?, session?)` | Current call stack (≤50 frames). |
| `dap_scopes(frame_id, session?)` | Scopes for a frame (Locals, Registers, …). |
| `dap_variables(variables_reference, session?)` | Read variables. |
| `dap_evaluate(expression, frame_id?, session?)` | Evaluate in the debuggee. |
| `dap_terminate(session?)` | Disconnect session. |

## Indexing

| Tool | What it does |
|---|---|
| `index_commit(path?, sha?)` | Force-enqueue a commit (HEAD by default) for SCIP indexing. |
| `index_status(path?, sha?)` | State: `Pending` / `InProgress` / `Ready` / `Failed`. |
| `work_last_known_state(path?)` | Last commit the indexer has Ready for this repo. |

## GitHub integration

Token resolution walks `$GITHUB_TOKEN` → `gh auth token` subprocess →
`~/.aide/auth/github.token` (chmod 0600). If all three miss, the write
tools error with a three-step actionable remediation; `gh_auth_status`
returns `{source: "none", remediation: …}` instead of erroring. No
OAuth of our own — device flow (variant C) is deferred pending a
GitHub OAuth App ownership decision.

| Tool | Replaces | What it does |
|---|---|---|
| `gh_auth_status()` | `gh auth status` | Walks the token waterfall, hits `/user` with the resolved token, returns `{source, login, scopes}`. Scopes from `x-oauth-scopes` header (empty for fine-grained tokens). Never errors — agents branch on `source`. |
| `gh_issue_create(title, body, labels?)` | `gh issue create` | Detects `:owner/:repo` from `origin` remote (SSH or HTTPS). Labels must already exist on the repo — GitHub does not auto-create. |
| `gh_issue_list(state?, labels?, limit?)` | `gh issue list` | `state` = open / closed / all. `labels` AND-join. `limit` mapped to `per_page` (max 100). Returns `[{number, title, state, html_url, labels}]`. |
| `gh_issue_view(number)` | `gh issue view` | Returns `{issue, comments}` in one call — issue includes full `body` and `state_reason`, comments are every reply in chronological order (up to 100). |
| `gh_issue_comment(number, body)` | `gh issue comment` | Post a comment. Use for "also hit this in commit X" / "duplicate of #N" instead of opening a second issue. |
| `gh_issue_close(number, reason?)` | `gh issue close` | `reason` = `completed` (default intent) or `not_planned`. Prefer `Closes #N` in a commit footer when the close is merge-driven — GitHub auto-closes and this tool is redundant. |
| `gh_ux_gotcha(title, body, tool, param?)` | — | Policy wrapper over `gh_issue_create`: hardcodes the `ux-gotcha` label, prefixes `title` with the implicated tool, appends a provenance footer. Use whenever dogfooding surfaces a trap. See CLAUDE.md § "Reporting UX gotchas". |

## Dogfood

| Tool | What it does |
|---|---|
| `dogfood_coverage_gaps` | Aggregate across `dogfood/runs/*.md`; rank the most-common "Bash call with no aide equivalent" bullets by frequency and recency. Feeds the v0.14 dogfood → roadmap loop. |

---

## Semantic model (read before querying)

Two data planes, two different trust boundaries:

**LSP — live working tree.** The language server sees the filesystem as
it is, including editor-buffered edits if your client pushes them. Use
for hover, definition, refs on the code you're actively editing. First
query after opening a workspace often returns `null` — the server is
still indexing; retry in ~30 s.

**SCIP — committed snapshot.** Keyed by commit SHA. Built post-hoc by a
background tokio worker against a fresh TempDir checkout of the commit,
not against the dirty tree. Use for call graphs, public-API maps, impact
analysis — anything where you need "the last known-good structure of the
codebase."

**Rule of thumb:** _What does this thing I just typed do?_ → LSP. _Who
calls X, what tests cover Y, what does the public API look like?_ →
SCIP.

Indexing is agent-driven: `project_setup` enqueues HEAD at the end,
every `git_*` tool enqueues HEAD after running (cheap no-op when
already Ready), and `index_commit` forces a refresh explicitly.

## Execution model

- Tools operate directly against the user's working tree. No worktree
  sandbox.
- SCIP is always built against an exported commit snapshot, never the
  dirty tree.
- Binaries live in `~/.aide/bin/` (globally, not per-project); manifest
  at `~/.aide/bin/manifest.json`.
- State at `~/.aide/queue/indexer_state.json` survives MCP restarts;
  Ready `.scip` files remain usable.
- Retention defaults to 1 Ready commit (latest HEAD); older commits are
  evicted on `Store::mark_ready`.
- `AIDE_HOME` env var overrides the root; `$HOME` stays untouched so
  rustup/cargo keep working.

---

## For AI agents

If you're a Claude/LLM agent with aide wired up:

- Prefer aide tools over Bash equivalents. The **Replaces** column above
  is your decision table.
- Prefer aggregates (`task_context`, `project_map`) over stitching three
  or four primitive calls together.
- `project_grep` already annotates hits with the enclosing SCIP symbol
  when an index is Ready — skip the manual "find enclosing fn" step.
- Before semantic queries, check `index_status` or
  `work_last_known_state`. If no Ready index, either fall back to LSP
  (live) or call `git_status` to nudge the worker and poll.
- `safe_edit` over raw `Edit` when the change touches compiler-visible
  contracts — you get the diagnostic delta for free.
- `run_tests` returns structured diagnostics; don't re-parse raw stdout.
- Project conventions for this repo live in [CLAUDE.md](../CLAUDE.md);
  roadmap state in [STATUS.md](../STATUS.md).

## Links

- [README.md](../README.md) — project overview
- [STATUS.md](../STATUS.md) — roadmap, versions, decision log
- [CLAUDE.md](../CLAUDE.md) — project conventions for AI
