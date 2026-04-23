# Task NNN: <title>

Date: YYYY-MM-DD
Slug: `<kebab-case>`

## Prompt (identical for both agents)

```
<exact text handed to both dogfood-vanilla and dogfood-aide>
```

## Ground truth

<what the correct answer actually is, filled in after spot-verification>

## Metrics

|                 | vanilla | aide  |
|-----------------|---------|-------|
| tool_calls      |         |       |
| aide_calls      |   —     |       |
| fallback_calls  |   —     |       |
| rule_violations |   —     |       |
| wall_s_measured |         |       |
| output_kB_est   |         |       |
| false_leads     |         |       |
| correct         |  ✓/✗    | ✓/✗   |
| completeness    |  1–5    | 1–5   |
| confidence      | low/med/high | low/med/high |

## Vanilla result (summary)

<1–3 sentences>

## Aide result (summary)

<1–3 sentences>

## Verdict

**Winner:** vanilla / aide / tie
**Reason:** <rychlost / přesnost / čistota / úspora contextu>
**Delta:** `aide − vanilla: ΔT=±N calls, ΔW=±M s, ΔB=±K kB`

## Follow-up change

- Commit: `<sha>` or `none`
- Files touched: `<…>`

## Coverage gaps (from aide agent)

<copy the `Coverage gaps` section verbatim from the aide agent's output.
If the agent violated the prompt by omitting it, synthesise the gaps from
the non-aide calls in its trail and mark them `(grader-filled)`>

## Notes

<friction, surprises, which aide tool replaced which vanilla pattern,
 agent prompt-compliance issues (rule violations, skipped sections)>
