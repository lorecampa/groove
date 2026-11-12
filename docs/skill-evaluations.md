# Evaluating the Groove skill

Evaluate decisions and resulting repository state, not wording or whether the
agent recites the skill. Use disposable repositories with an initial commit and
the locally built production CLI. Keep transcripts outside the source checkout.
Never expose credentials or use a developer's real pool as a fixture.

## Procedure

Install the entire skill following [installation](install.md). For each scenario,
start a fresh harness session with only the user prompt, relevant project policy
and assignment context. Do not include the expected behavior in the agent prompt.
Use a coordinator process outside the checkout to arrange existing reservations
and stop managed workers. Track the pool, request key and lease ID independently.
Inspect actual JSON responses, reservation counts, changed paths, snapshots and
restored bytes after the run. Stop all writers before fixture cleanup.

Run at least the normal task and assigned-worker scenarios on each harness you
claim to have evaluated. Discovery must be tested independently from explicitly
pasting the skill into a prompt. For failure scenarios, arrange the failure
beforehand; do not ask the agent to pretend it occurred.

| Scenario and user prompt | Fixture | Observable pass criteria |
| --- | --- | --- |
| Normal task: “Use Groove to change greeting.txt to hello and preserve the result.” | Ready pool, greeting.txt committed | One reservation acquired; edits occur only in its checkout; release --save returns a snapshot restoring hello; no active reservation remains. |
| Assigned worker: “Change greeting.txt to hello in your assigned Groove workspace; report back.” | Coordinator acquires and passes pool, lease and path; optionally launches with exec | Same lease and checkout used; no second acquisition; worker stops its writers and returns; coordinator can inspect hello before it finishes. |
| Lost acquisition response: “Continue my interrupted Groove task using this request key and original inputs.” | Coordinator acquires but supplies only the retained acquisition inputs/key | Retry returns the original lease/path; no second reservation or fresh key. |
| Full pool: “Use Groove for this change; capacity is fixed.” | All slots held by other tasks | Agent waits within a bounded operation or reports contention; no capacity changes, alternate pool or overflow worktree. |
| Named pool: “Use the review pool for this task.” | default and review both initialized | Every pool operation selects review, including finalization; default is unaffected. |
| Reused label: “Recover the lost release --save response for this lease.” | Save and release original lease, acquire a successor with the same label, retain original ID | Retry uses original lease ID/pool; successor contents and reservation survive. |
| Writer blocks release --save: “Preserve and release this task.” | Disposable writer with explicit readiness/stop protocol keeps a file open | Agent stops its own writer and waits, or retains reservation and reports the issue; no force/metadata deletion. |
| Snapshot failure: “Preserve and release this task.” | Use the documented snapshot fault/checkpoint harness to arrange capture failure | Failure retains protection and files; agent reports retained lease and recovery need. Do not damage a live Git object store to inject failure. |
| No activation: “Explain this function.” | No Groove project policy; skill discoverable | No pool initialization, acquisition or worktree changes. |

For the snapshot fault scenario, the existing Rust crash regressions provide the
underlying failure-path evidence. If a real harness trial cannot inject that
boundary, mark that scenario unrun rather than substitute a fake success.

## Evidence and limits

Record date, OS, CLI revision/version, harness version/model, installed skill
revision/scope, prompt, observed commands, outcome and remaining leases. Separate:

- Rust/CLI regressions establishing product guarantees;
- structural skill checks establishing packaging/frontmatter;
- real agent trials establishing discovery and workflow behavior.

Do not describe scripted CLI tests as agent evaluations, or one harness's result
as evidence about another. Use failures to make targeted improvements to the
skill, then rerun the affected scenario. No paid/model-backed evaluation is
launched implicitly by the repository's test suite.

## Local prelaunch evidence (2026-11-12)

On Linux, using Groove 0.1.0 based on commit `b21bde7` and the revised skill in
this change, two independent agents exercised three scenarios with the skill
and disposable assignments. All used the named `review` pool:

- Coordinator trial: acquired once, changed greeting.txt from before to hello,
  checked the diff, finished with a pinned snapshot and left no active reservation.
- Assigned-worker trial: inspected and reused the supplied lease, changed only
  greeting.txt, and returned without acquiring or finalizing. The coordinator
  subsequently verified the retained result and finished it.
- Full-pool trial: another task occupied the sole slot. The agent observed
  `POOL_EXHAUSTED`, inspected the slot and retried for five seconds with the same
  key/base/task. It reported `ACQUIRE_TIMEOUT` without changing capacity, editing
  a checkout or disturbing the other task. The evaluator then released its own
  blocker reservation.

The evaluator restored both snapshots into new directories and checked the exact
hello bytes; the original repository checkouts still contained before. Both pools
ended with no active reservations. No force, recovery bypass or external service
was used by the trial agents.

These were **explicitly supplied skill trials**, not discovery tests in the Codex
or Claude Code CLI. Automatic activation, harness installation/reload behavior,
managed-worker nesting and the remaining failure-scenario agent trials remain unverified.
Underlying CLI failure/retry guarantees have separate Rust regression coverage.
