---
name: dogfood
description: A/B benchmark for aide-mcp itself. Runs the same read-only research task through `dogfood-vanilla` (no aide tools) and `dogfood-aide` (prefers `mcp__aide__*`) agents in parallel, parses their metrics blocks, and records a scored run. Invoke when the user says "dogfood <task>", "benchmark aide on <task>", "compare aide vs vanilla", or wants to measure whether aide-mcp is actually helping. Read-only — vanilla output is authoritative for any follow-up code change, aide is reference-only.
---

# Dogfood benchmark

Measure whether `mcp__aide__*` tools beat plain Read/Grep/Bash on real
project questions. Two paired agents, identical prompt, structured scoring.

## Layout

Skill file lives under `.claude/skills/dogfood/SKILL.md` (required for
Claude Code to discover it). All **writable** artifacts — `INDEX.md`,
`TEMPLATE.md`, and `runs/NNN-*.md` — live under `dogfood/` at the repo
root, away from `.claude/` where sandbox writes trigger permission
prompts. Paths below are workspace-root-relative.

## When to invoke

Trigger phrases: "dogfood ...", "benchmark this: ...", "compare aide vs
vanilla on ...", "run the dogfood experiment". If the user invokes with
args (`/dogfood find AidePaths usages`), treat args as the task. Otherwise
ask.

If the task implies mutation ("refactor X", "add test Y"), narrow it to
the research half first — both agents are read-only by construction.

## Workflow

1. **Craft the prompt.** Write a single self-contained task prompt. State
   the question, the scope (file/module), the expected answer shape
   (citations? paragraph? list?). Show the prompt to the user and confirm
   before firing agents — this is the costly step.

2. **Slug & run number.** Short kebab-case slug. Next run number =
   highest existing in `dogfood/runs/` + 1, zero-padded to 3 digits.

3. **Record start time.** `date +%s` via Bash.

4. **Spawn BOTH agents in ONE message** (parallel):

   ```
   Agent(subagent_type: "dogfood-vanilla",
         description: "vanilla: <slug>",
         prompt: "<task>")
   Agent(subagent_type: "dogfood-aide",
         description: "aide: <slug>",
         prompt: "<task>")
   ```

5. **Record end time.** Wall clock for the slower of the two.

6. **Parse metrics blocks.** Each agent ends with a fenced ```metrics
   block. Extract `tool_calls`, `wall_s_estimate`, `output_kB_estimate`,
   `false_leads`, `confidence`. Aide additionally reports `aide_calls`,
   `fallback_calls`, and `rule_violations` (count of Bash calls that
   hit a forbidden pattern the aide agent was told to avoid). Prefer
   your measured wall clock over the self-reported estimate.

7. **Audit aide agent compliance.** Quickly scan the aide tool trail:
   - Count forbidden-pattern Bash calls (`ls`, `cat`, `head`, `tail`,
     `grep`/`rg`, `git log`/`diff`/`blame`/`status`, `wc`, `awk`,
     `sed`). Compare to the agent's self-reported `rule_violations`.
     If the agent undercounted, note the grader-adjusted number in
     the run record.
   - Check that the agent included a **Coverage gaps** section. If
     omitted, synthesise gaps from the non-aide calls and mark
     `(grader-filled)` in the run record.
   Non-compliance is reported but does not disqualify the run — it
   just goes in the Notes section and informs prompt tightening.

8. **Spot-verify vanilla.** Read 1–2 cited `[path:line]` refs to check
   vanilla did not hallucinate. If vanilla is wrong, mark `correct: ✗` —
   but still keep it as the authoritative output for the record (the
   experiment is honest about misses).

9. **Fill the run record.** Copy `dogfood/TEMPLATE.md` →
   `dogfood/runs/NNN-<slug>.md`, fill every section. Summaries 1–3
   sentences. Include the exact prompt. Paste the aide agent's
   `Coverage gaps` section verbatim into the new `Coverage gaps` block
   of the template — these bullets are the project's backlog signal
   for new aide tools.

10. **Update `dogfood/INDEX.md`.** Append one row. Recalculate the
    aggregate block (tasks run, wins, ties, mean deltas).

11. **Report to user.** One paragraph: winner, why, any surprising
    finding. Point at `dogfood/runs/NNN-<slug>.md`. Surface any
    aide-tool coverage gaps the run uncovered — they are backlog
    candidates.

## Scoring rubric

- **Aide wins** — fewer tool calls with equal correctness, OR caught
  something vanilla missed, OR materially cleaner evidence.
- **Vanilla wins** — fewer tool calls with equal correctness, OR aide
  was wrong / misleading.
- **Tie** — comparable on both axes.

Always record a delta line:
`aide − vanilla: ΔT=±N calls, ΔW=±M s, ΔB=±K kB`
(positive = aide was more expensive).

## Hard rules

- Never apply aide agent's output to the repo. Only vanilla's answer
  drives follow-up changes (done outside this skill).
- Never re-run a task silently. A retry gets a fresh run number.
- Never skip metrics parsing — that is the point.
- Never invoke `dogfood-vanilla` / `dogfood-aide` outside this skill
  for non-benchmark purposes; they will pollute the dataset.
