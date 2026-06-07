# aide-mcp vs. serena

[Serena](https://github.com/oraios/serena) is the closest competing MCP
toolkit — same problem statement ("give agents IDE-grade code
intelligence"), different design choices. This page lays out where the
two projects overlap, where aide is ahead, and where serena is ahead.
Written 2026-06-07 against serena ~v1.5.x and aide-mcp v0.22.

This is descriptive, not a roadmap. New aide features still belong in
[STATUS.md](../STATUS.md) and only land when the dogfood loop justifies
them.

## TL;DR

- **serena = broad LSP wrapper.** Python-based, 40+ languages via
  off-the-shelf LSP servers, optional JetBrains backend for refactoring
  + debugging, first-class symbolic-editing primitives, memory and
  onboarding tools, contexts/modes configuration system.
- **aide-mcp = narrow but deep hybrid.** Rust single binary, 6
  languages, LSP for live state **and** SCIP for commit-snapshot
  semantic queries, DAP debugging without IDE dependency, git +
  GitHub workflow as MCP tools, structured cargo/test diagnostics
  annotated with enclosing SCIP symbol.

## Architecture at a glance

| Dimension | aide-mcp | serena |
|---|---|---|
| Implementation language | Rust | Python (89.9 %) |
| Distribution | Single static binary | `uv tool install serena-agent` |
| Transport | stdio only | stdio + HTTP |
| Backend(s) | LSP (live) + SCIP (committed) | LSP, optional JetBrains plugin |
| Symbol index | SCIP protobuf, keyed by commit SHA, persisted under `~/.aide/scip/` | Two-tier in-memory LSP document-symbol cache; optional pre-index command |
| Stable snapshot | Yes — each commit produces a frozen `.scip` | No — symbol cache invalidates with edits |
| Tool install | `project_setup` auto-downloads LSP/SCIP/DAP binaries into `~/.aide/bin/` | User installs language servers manually |
| Languages | Rust, Java (Maven/Gradle), Node/TS, Python, Go, C/C++ — 6 ecosystems | 40+ languages via generic LSP plumbing |
| Debugging | DAP first-class (codelldb for Rust + C/C++) | Only via JetBrains plugin backend |
| Git / GitHub | `git_*` read ops + `gh_*` issues/PRs/checks as MCP tools | Not in scope (relies on `execute_shell_command`) |
| Editing model | LSP rename + `lsp_apply_code_action` + `safe_edit` (diagnostic-delta wrap) | Symbolic primitives (`replace_symbol_body`, `insert_before/after_symbol`), line-range ops, regex `replace_content`, LSP rename |
| Memory | None inside MCP — relies on the host (Claude Code globals) | First-class `read/write/list/delete/rename/edit_memory` tools |
| Config | Single `~/.aide/config.toml` (hot-reloaded) | Layered: global / CLI / per-project / contexts / modes |
| Dashboard | None | Web dashboard |

## Where aide-mcp is ahead

### 1. SCIP-on-commit hybrid, not just LSP

Serena's "index" is a smarter LSP cache: faster, gitignore-aware, but
still tied to live workspace state. aide builds **per-commit SCIP
indexes** keyed by SHA. That unlocks queries serena can't answer:

- `scip_references(symbol, sha)` against a historical commit
- `public_api_diff(sha1, sha2)` — structured pub-surface delta
- `tests_for_symbol` / `tests_for_changed_files` over the SCIP call
  graph rather than name guessing
- `impact_of_change(symbol)` with callers classified as
  test / bin / lib / example / bench
- `project_grep_at` / `project_ls_at` reading directly from git trees
  with no checkout
- `enclosing_symbol` annotation on every grep hit, every compiler/test
  diagnostic, every reference / definition / safe_edit result —
  derived from the latest Ready SCIP

The cost is real (each commit re-runs the language's SCIP indexer)
but the analytic ceiling is much higher.

### 2. Debugging without an IDE

aide ships DAP over stdio: `dap_launch`, breakpoints,
continue/step, stack/scopes/variables, `dap_evaluate`. Works against
codelldb for Rust and C/C++ today, designed to grow per language.

Serena's debugging exists only through the **JetBrains plugin** —
i.e. it requires a running JetBrains IDE. Agents working over SSH,
in CI, or in headless environments are out.

### 3. Auto-install of language tooling

`project_setup` detects languages and downloads LSP / SCIP / DAP
binaries into `~/.aide/bin/` with version pinning and an idempotent
`manifest.json`. Zero "first install `rust-analyzer` and add it to
PATH" friction.

Serena assumes the language server is already installed and
discoverable; setup burden falls on the user.

### 4. Git + GitHub as first-class MCP tools

- `git_status / git_log / git_diff / git_blame` — structured JSON, not
  text-parsing of shell output
- `gh_issue_create / list / view / comment / close` plus
  `gh_pr_create / list / view / checks`
- `gh_ux_gotcha` — policy-as-code: hardcoded label + body template so
  the dogfood-friction reporting loop can't drift

Serena delegates all of this to `execute_shell_command`.

### 5. Structured diagnostics from cargo/test

`run_project` / `run_tests` / `install_package` parse the plugin's
JSON output and return `diagnostics: Vec<StructuredDiagnostic>` with
`{level, code, message, file, line, enclosing_symbol, spans,
suggested_fix}`. The agent doesn't re-derive what `error[E0382]`
means from a wall of text.

Serena returns shell output verbatim.

### 6. Higher-level aggregate tools

One MCP call instead of five: `task_context(file)`,
`project_map(path?)`, `scip_callers(symbol)`, `tests_for_symbol`,
`impact_of_change`, `public_api_diff`. These exist precisely because
the dogfood benchmark kept showing agents stitching together five
tool calls to answer one question.

Serena exposes the LSP primitives directly and trusts the agent to
compose them.

### 7. Operational ergonomics

- Single Rust binary, ~instant startup, no Python runtime to manage.
- Job management (`job_list`, `job_info`, `job_kill`) scoped to
  aide-spawned processes only — no path to signal arbitrary PIDs.
- `process_list` for read-only sysinfo without shelling out to `ps`.
- `read_exec_log` for tailing long-running output.
- MCP progress notifications on long-running exec calls.

### 8. Macro / generated-code visibility

`lsp_expand_macro` exposes rust-analyzer's `expandMacro` — the
single biggest win in macro-heavy crates. Serena has nothing
language-specific here.

## Where serena is ahead

### 1. Language breadth

40+ languages vs. aide's 6. If you work in Ruby, Kotlin, Scala,
Swift, Lean, OCaml, Elixir, PHP, Haskell, Solidity, Zig, … — serena
is the only option today. aide's `LanguagePlugin` trait is the
on-ramp, but each new language requires LSP + SCIP + (eventually)
DAP wiring and bin-install plumbing.

### 2. Symbolic editing primitives

Serena offers `replace_symbol_body`, `insert_before_symbol`,
`insert_after_symbol`, `safe_delete_symbol`, symbol-addressable by
`name_path`. These map cleanly to how an LLM thinks about edits ("put
this implementation in `Foo::bar`") without having to compute line
ranges.

aide today goes through `lsp_rename_symbol`,
`lsp_apply_code_action`, and `safe_edit` (which is line/column-based
with a unique `old_string`). The symbolic-edit primitive is missing
and is a real gap for write-heavy agent work.

### 3. Memory as MCP tools

`write_memory`, `read_memory`, `list_memories`, `delete_memory`,
`rename_memory`, `edit_memory` — exposed directly to the agent.
Cross-session, cross-project, cross-user knowledge persistence
without a host-specific layer.

aide currently relies on the host's memory (e.g. Claude Code's
`~/.claude/projects/.../memory/`). That works for Claude Code but
not for portable agent workflows.

### 4. JetBrains backend alternative

Power users get JetBrains-quality refactorings (move, inline,
safe_delete, type-hierarchy) and interactive debugging by attaching
to a running JetBrains IDE. aide has no equivalent escape hatch.

### 5. More LSP query primitives exposed

- `find_implementations(symbol)`
- `find_declaration(symbol)` (distinct from definition)
- type hierarchy

aide exposes hover / definition / references / document symbols /
workspace symbols / diagnostics. The three above would be cheap
wins if a dogfood run flags them.

### 6. Onboarding + workflow tools

`OnboardingTool`, `InitialInstructionsTool`, `SerenaInfoTool`,
`ActivateProjectTool`. Designed for the moment an agent first
arrives at a project: discover structure, learn conventions, get
the usage manual. aide assumes a host that already knows what it
wants.

### 7. Multi-project querying

`QueryProjectTool` runs read-only queries against projects other
than the currently active one. Useful for monorepos and
cross-project navigation. aide is single-project-rooted per tool
call (each tool accepts a `path` arg, but there's no aggregate
"query everything I have configured" tool).

### 8. Contexts and modes

Composable configuration fragments per client and per workflow.
aide has one TOML file; serena lets you swap toolsets per integration
(Claude Code vs. Codex vs. JetBrains plugin).

### 9. HTTP transport

Serena can run as an HTTP MCP server, useful for remote / shared
deployments. aide is stdio only.

### 10. Web dashboard

Browser UI for monitoring tool calls, viewing config, project
selection. aide has no UI surface.

## Rough parity

Both projects cover:

- LSP hover / definition / references / symbols / diagnostics
- LSP-backed cross-file rename
- Project-wide search (aide's `project_grep` ≈ serena's
  `search_for_pattern`)
- Directory listing (aide's `project_ls` ≈ serena's `list_dir` +
  `find_file`)
- Diagnostic-aware edits (aide's `safe_edit` ≈ serena's
  `EditingToolWithDiagnostics` base)

## Summary table — pick-me criteria

| If you care most about… | Pick |
|---|---|
| Working in Rust, Java, Node/TS, Python, Go, or C/C++ | aide-mcp |
| Working in 30+ other languages today | serena |
| Stable semantic snapshots across commits, impact analysis, public-API diff | aide-mcp |
| Symbolic editing primitives keyed by symbol name | serena |
| Headless debugging without a desktop IDE | aide-mcp |
| Single binary, zero Python deps, auto-installed language tooling | aide-mcp |
| Built-in agent memory + onboarding + multi-project queries | serena |
| Git + GitHub workflow inside MCP | aide-mcp |
| HTTP transport / shared MCP server / web dashboard | serena |
| Structured cargo/test diagnostics with enclosing symbol | aide-mcp |
