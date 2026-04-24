# Task 001: Node.js/TypeScript plugin plan (3rd attempt)

Date: 2026-04-23
Slug: `nodejs-plugin-plan`

## Prompt (identical for both agents)

```
Investigate what it would take to add Node.js/TypeScript language support to aide-mcp. Identify:

(1) the LanguagePlugin trait contract and where plugins are registered,
(2) how existing language plugins (Rust, Java) are structured — files touched, tools pinned, toolchain detection,
(3) which LSP server(s) and SCIP indexer would be needed for Node/TS and how they'd be pinned per the "tool binaries are always pinned" invariant,
(4) concrete list of files to create/modify and their responsibilities.

Read-only research — do not write any code. Return a concise plan that a follow-up implementation task could execute from. Cite every claim as [path:line]. End with the required ```metrics block.
```

## Ground truth

Spot-checked citations:

- `crates/aide-lang/src/plugin.rs:65-104` — trait decl ✓
- `crates/aide-lang/src/registry.rs:19-27` — `builtin()` ✓
- `crates/aide-mcp/src/indexer/worker.rs:96-103` — `registry.detect()` then `plugin.scip()` ✓
- `crates/aide-lsp/src/ops.rs:302-311` — already maps `ts/tsx/js/jsx` ✓

Both plans converge again on the same plan: one new `crates/aide-lang/src/languages/node.rs` (or `typescript.rs`), `pub mod` line in `languages/mod.rs`, registration push in `Registry::builtin()`, using `Source::DirectUrl` + `ArchiveFormat::TarGz` + `custom_install` wrapper (JDT-LS pattern). Pins: `typescript-language-server`, `typescript`, `scip-typescript`. DAP = `None`. Node on `$PATH`.

## Metrics

|                 | vanilla | aide  |
|-----------------|---------|-------|
| tool_calls      |  28     |  14   |
| aide_calls      |  —      |   0   |
| fallback_calls  |  —      |  14   |
| rule_violations |  —      | omitted (grader: ≥1 confirmed, see Notes) |
| wall_s_measured | 158     | 152   |
| output_kB_est   |  95     |  55   |
| false_leads     |   0     |   0   |
| correct         |   ✓     |   ✓   |
| completeness    |   5     |   4   |
| confidence      |  high   | high  |

## Vanilla result (summary)

Full, well-cited plan. Single new file `crates/aide-lang/src/languages/typescript.rs`, two edits (`mod.rs`, `registry.rs`), docs bump. Calls out JDT-LS `install_jdtls_wrapper` as the direct template for Node's `node <cli.js>` wrapper. Notes that the install engine already covers npm tarballs and no new archive variant is needed.

## Aide result (summary)

Same plan, also correct. Recommends `NodePlugin` name. Explicitly flags two risks to verify during implementation: whether `scip-typescript` ships a runnable GitHub artifact or needs `npm install` at `custom_install` time, and that the wrapper must pass `--tsserver-path=…/node_modules/typescript/lib/tsserver.js`.

## Verdict

**Winner:** vanilla (on benchmark legitimacy; plans themselves are tied)
**Reason:** Vanilla produced a thorough, methodical plan. Aide's plan
is equally correct but `aide_calls: 0` and multiple compliance lapses
(missing `rule_violations` metric, missing Coverage gaps section,
forbidden Bash patterns still in the trail) make this a non-compliant
aide-side run. On the research question itself the two plans are
essentially tied.

**Delta:** `aide − vanilla: ΔT=-14 calls, ΔW=-6 s, ΔB=-40 kB`
(aide cheaper — but by not doing the aide part of the benchmark)

## Follow-up change

- Commit: `none` (research-only)
- Files touched: none

## Coverage gaps (grader-filled — agent omitted the section)

- `list files in a directory` → `aide_project_ls(prefix, glob?)` — aide
  currently has no directory-enumeration tool, only `scip_documents`
  which is constrained to indexed paths.
- `free-text grep across repo` → `aide_project_grep(pattern, path_glob)` —
  gitignore-aware, fixed to the project root.
- `grep for a symbol definition / callsites` → already covered by
  `lsp_workspace_symbols` / `lsp_references` / `scip_references` / `scip_symbols`
  (agent did not use them — **agent error, not a coverage gap**).

The first two bullets directly motivate the planned
`project_ls` / `project_grep` sandboxed shell primitives (see project
memory).

## Notes

### The tightened prompt over-corrected

Between the discarded run and this one, `dogfood-aide.md` was tightened
with HARD RULES, a pre-flight checklist, and a new required
`rule_violations` metric. The agent response was the opposite of the
previous run — instead of using aide tools more and still Bash-ing
occasionally, it used **zero** aide tools and declared the task doesn't
fit aide's strengths. `project_detect` alone would have been trivially
applicable (and was used in earlier attempts). The agent's "aide value
note" argument is partly defensible for this task shape, but skipping
aide entirely is not the benchmark's intent.

### Compliance lapses

- `rule_violations` field was **omitted** from the metrics block even
  though it is now a required key.
- **Coverage gaps section was omitted** even though the updated prompt
  requires it to be present (with explicit "none" formulation if
  empty). Grader filled it above.
- Tool trail labels everything `[fallback]` with ambiguous phrasing
  (e.g. "grep server.rs for plugin/registry refs") that does not make
  clear whether the Grep tool or `Bash(grep)` was used. Only
  "Bash find+ls to enumerate crates/plugins" is unambiguously a
  forbidden-pattern Bash violation. Grader-adjusted `rule_violations`: ≥ 1.
- No tool-type prefix (`Read(...)`, `Grep(...)`, `Bash(...)`) on trail
  entries — violates the Pre-flight checklist item 4.

### Task-shape implication

Two consecutive runs on this task now show the same pattern: the aide
agent correctly recognises that "map this architecture by reading five
files" is not where `lsp_references` / `scip_symbols` help. The
benchmark's next move should be a task where aide is designed to win:
symbol-tracing, cross-file reference walks, diagnostics-driven
questions. This run's real value is the Coverage gaps — both the
backlog bullets and the agent-compliance data for the next prompt
iteration.

### Prompt-tightening next steps

- Require the `rule_violations` metric to be physically present in the
  metrics block — grader can auto-reject runs missing it.
- Require the Coverage gaps section with the literal heading
  `## Coverage gaps` before the metrics fence — same enforcement.
- Consider adding a "minimum aide calls" floor (e.g. at least
  `project_detect` at the start) purely to prove the agent read the
  ruleset — though this risks teaching the agent to fake calls rather
  than use them meaningfully.
