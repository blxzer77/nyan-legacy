# Agent Note: Implementation plan for the review stack, test evidence, and pipeline

Status: proposed. This is a plan; nothing in it is implemented.

Decisions and the evidence behind them are in
[the decisions note](2026-10-19-test-quality-and-review-pipeline.md). This note orders the work. Step
numbers here are the only numbering; the decisions note's "Gaps" and "Open questions" are referred to by
name.

## How the plan is executed

- The order is strict. One step is one short-lived branch cut from an up-to-date `main`, and one change that
  can be reverted alone. A step is finished when its "Done when" evidence exists, not when the edit is written.
- A step marked **[GH]** can only be proven by a run on GitHub (a workflow proves itself nowhere else), so it
  needs a push.
- **Authorization.** Nothing is committed, pushed, tagged, or published, and no pull request is opened, without
  the owner's explicit say. Changes to repository settings, secrets, environments, the runner host, and token
  rotation are owner-authorized actions even when the command is run here.
- **Roadmap.** `ROADMAP.md` allows two active lines and the two slots are held by N2 and Bun Phase 3. This
  work is a third line unless the owner folds it into an existing one or lifts the rule for it. Each step that
  finishes updates `ROADMAP.md` in the same change.
- **Evidence wording.** Every step reports what ran and what it showed, and lists what stays unverified.

## Order

| # | Id | Step | Kind | Blocked by |
| --- | --- | --- | --- | --- |
| 1 | R1 | Review standard: test rules in `AGENTS.md` and OCR project rules | repo only | unknown U1 for the OCR part |
| 2 | P1 | Release guard: tag-only publish, manual run is a dry run, `npm` environment restricted | repo + settings, [GH] | owner authorization for settings |
| 3 | P5 | Runner labels by purpose, nightly moved to local night | host + repo, [GH] | owner access to the host |
| 4 | R2 | PR-Agent `/describe` on same-repository pull requests | new workflow, [GH] | step 3, U3, U4 |
| 5 | R3 | OCR on `ready_for_review`, manual, and before a release, fed the summary | workflow, [GH] | steps 1 and 4, U1, U2 |
| 6 | P2 | Ship `rg` and `fd` in the workspace archive | workflow, [GH] | none |
| 7 | T1 | Persist the Stryker incremental file | workflow, [GH] | none |
| 8 | R4 | Gateway service in `blxzer77/nyan-gateway` (started) | new service | steps 4 and 5 for data |
| 9 | R5 | Langfuse traces, golden review cases, evaluators | service + repo | step 8, U6 |
| 10 | R6 | Reflector | service | steps 8 and 9 |
| 11 | P3 | Windows and MCP run by impact; gate list generated from `policy.json` | workflow, [GH] | none |
| 12 | P4 | Release preflight: same-commit `push` gate on `main` | workflow, [GH] | step 2 |
| 13 | T2 | Mutation scope beyond `cbor/**`; property tests | tests | step 7 |
| 14 | H | Hygiene batch (see the end) | mixed | none |

Why this order: R1 and P1 need no infrastructure and carry the most value and the most risk reduction. P5 is
the one prerequisite of putting any pull-request work on the self-hosted host. R2 and R3 are the owner's goal
and are built first on a direct CPA connection, so the gateway's profiles come from measured behaviour. P2 and
T1 are small measurable wins. R4 to R6 are the large service work. P3 and P4 are correctness fixes with no
release due (the roadmap's stop condition forbids one for now). Steps 6, 7, 11, 12 do not depend on R4 and may
move ahead of it if the models' instability bites less than expected.

## Unknowns that block steps

| Id | Unknown | Resolved by |
| --- | --- | --- |
| U1 | Where and in what format OCR reads project rules | **Resolved 2026-10-19**, see below |
| U2 | Whether the dispatch accepts `background` and rules | **Partly resolved 2026-10-19**, see below |
| U3 | Whether the `describe` output of the pinned PR-Agent version can be made to satisfy our summary schema | Run it on a sample diff; see R2 |
| U4 | CPA endpoint and the credential name PR-Agent uses | Owner |
| U5 | Where the gateway's code lives | **Resolved 2026-10-19:** `blxzer77/nyan-gateway`, its own repository |
| U6 | Langfuse endpoint and key names | Owner |

## Resolved unknowns (2026-10-19)

Read from `alibaba/open-code-review` (Apache-2.0, v1.13.1, `ocr` CLI, npm package
`@alibaba-group/open-code-review` plus standalone binaries) and its `pages/src/content/docs/en/review-rules.md`.

- **Project rules live at `<repo>/.opencodereview/rule.json`**, a path that is committed and is the second of
  four layers (`--rule`, project, `~/.opencodereview/`, embedded system). Format:
  `{"include": [...], "exclude": [...], "rules": [{"path": glob, "rule": text, "merge_system_rule": bool}]}`.
  The first matching `path` in declaration order wins. `merge_system_rule: true` keeps the built-in per-language
  rule and adds ours; without it our rule replaces the system one. Globs are doublestar, matched
  case-insensitively. `ocr rules check <file>` prints which layer and pattern won.
- **Test files are excluded by default.** The built-in exclusion list contains `**/*.test.{js,jsx,ts,tsx}`,
  `**/spec/**`, `**/test/**`, `**/__tests__/**`, and `**/fixtures/**`. Reviewing test quality therefore requires
  an `include` entry, because `include` is the only switch that bypasses those gates. This is the single most
  important configuration fact for the owner's goal: without it the reviewer never sees a test.
- **`--background` is capped.** 2 000 characters recommended, 8 000 hard, and a `--background-file` at most
  1 MB; the text is wrapped in `<ocr_user_background>` and stripped of control characters. The summary schema
  must therefore fit 8 000 characters, which is a real constraint on summary design and the reason the summary
  must be structured rather than prose.
- **Still unknown (U2):** whether the dispatch service passes `--background` and `--rule` through. Its HTTP
  payload is ours, so this is a change to that service if it does not; the repository cannot show it.

The rule file and the `AGENTS.md` tests section are therefore unblocked and are step 1. The finding schema moves
from step 1 to step 8, where something reads it; adding an unused schema now would be dead weight.

## Steps

### 1. R1: review standard

Change:
- `AGENTS.md`: a short "Tests" section of one-sentence rules. A test derives from the public contract
  (signature, inputs, outputs, pre and post conditions, documented errors), not from the implementation. Table
  of cases for pure logic. No mock of an internal module or private function; a fake only at a real boundary
  (provider, filesystem, clock, network). Assert exact values, not "is defined". Cover the boundaries and the
  error and recovery paths named in the contract. A test that cannot fail when the code is wrong is not
  written. Read the existing `nyan-checks` skill so no rule is duplicated.
- OCR project rules, in the location and format found under U1: the rubric categories (tautological test, weak
  assertion, over-mocking, missing boundary, missing recovery or restart path, concurrency, state ownership,
  architecture rules, Pi names, default URLs). The categories come from `.agents/skills/nyan-code-review/SKILL.md`,
  which stays the single source; the OCR file links or is generated from it rather than copying it.
- `.ci/ai-review/schemas/finding.schema.json`: category, severity, confidence, file, line, evidence,
  `suggested_test`, `validation_command`.

Done when: `npm run check` exits 0 (it lints the JSON), and the rules read back correctly from the files. The
OCR part is verified in step 5, not here.
Rollback: revert the commit; no runtime behaviour changes.

### 2. P1: release guard

Change:
- `release.yml`: remove the `dry_run` input; a `workflow_dispatch` run is always a dry run. Real publish and the
  GitHub release run only for a tag matching `^nyan-v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$`, and the job
  refuses otherwise.
- Split `publish` in two: a tag-only job that uses `environment: npm`, and a dry-run job with no environment
  and no `NPM_TOKEN`. A tag-only environment would otherwise block every branch dry run.
- Update the `AGENTS.md` Release paragraph ("defaults to a dry run" becomes "is always a dry run") and the
  header comment of the workflow.
- Settings, owner-authorized: restrict the `npm` environment to tags `nyan-v*`; move `NPM_TOKEN` from a
  repository secret into that environment.

Done when: `npm run check:workflows` passes; **[GH]** a branch dispatch finishes green without reaching
`npm publish`; **[GH]** the same dispatch cannot start the `npm` environment job. Unverified until a real tag:
the tag path itself.
Open point: whether `npm publish --dry-run` needs a credential. If it does, the dry-run job needs a read-only
path; test it before relying on "no token".
Rollback: revert the workflow commit; restore the secret and the environment policy.

**Status 2026-10-19: repository part done in [PR #79](https://github.com/blxzer77/nyan/pull/79).** The `dry_run`
input is gone, `publish` is tag-only and re-checks the tag, and `publish-dry-run` runs a dispatch with no
environment and no token. Verified: `npm run check` and `check:workflows` pass; run `39234910118` (a branch
dispatch) is green, skipped the tag-only `publish` job, and printed `Dry run complete.` after integrity-checking all
12 tarballs with no token in the step environment. The credential open point is resolved: `npm publish --dry-run`
exits 0 with `HOME` empty and `NODE_AUTH_TOKEN`/`NPM_TOKEN` unset, warning only that a login would be required.
Still owner-gated: the `npm` environment must be restricted to `nyan-v*` tags and `NPM_TOKEN` moved into it; until
then the workflow-level tag check is the only guard. A tag push remains unverified.

### 3. P5: runner labels and schedules

Change (host steps need the owner):
- Labels by purpose across the six runners, proposed 3 `nyan-test`, 1 `nyan-review`, 2 `nyan-batch`. Nightly
  width drops to 2 so the pool never exceeds 3 + 2 + 1 jobs. The cost is a longer nightly (9.1 min measured on
  five slots); the gain is that PR shards and a nightly cannot share the cores.
- `ci-shards.mjs`: `SELF_HOSTED_RUNNER` becomes `nyan-test`; `nightly.yml` and `ocr-review.yml` use their own
  labels. `ci-shards.test.mjs` imports the constant, so its assertions still hold.
- `nightly.yml` cron to a local-night UTC time (for example `17 16 * * *`, 00:17 at UTC+8). Scheduled runs
  start 2 to 9 hours late here, so the aim is "not during the day", not a start time.
- Host hygiene: rotate the PAT found in `/root/.npmrc`, clear any other credential from the runner users.

Done when: `npm run test:scripts`; **[GH]** a pull-request run places three shards on `nyan-test`, and a manual
nightly dispatch runs only on `nyan-batch`.
Rollback: restore the shared label; labels are metadata.

### 4. R2: PR-Agent

Change: `.github/workflows/pr-review.yml`, trigger `pull_request` (`opened`, `synchronize`, `reopened`,
`ready_for_review`).
- Guard: the job runs only if `github.event.pull_request.head.repo.full_name == github.repository`. It never
  checks out fork code. `pull-requests: write` is granted to this job alone; the workflow default stays
  `contents: read`.
- Runner `nyan-review`. PR-Agent as a container pinned by digest, calling CPA directly for now through the
  OpenAI-compatible base URL. Only `describe` is enabled; `review` and `improve` are off.
- One persistent, updated comment, not a new comment per push. Concurrency group per pull request cancels an
  older run.
- Summary schema (decision 21). PR-Agent's own `describe` output is its format, not ours. Test it on a sample
  diff (U3): if it can be coaxed into our fields, use it; if not, the human-readable comment stays PR-Agent's
  and a thin summary step produces the structured artifact that R3 consumes.

Done when: **[GH]** a same-repository pull request gets one comment that updates on a second push; **[GH]** a
fork pull request does not start the job (simulate with the guard expression, not a real fork).
Unverified: behaviour under a real fork, which this repository has none of.
Rollback: delete the workflow.

### 5. R3: OCR deep review

Change: `ocr-review.yml` gains `pull_request: [ready_for_review]` and a pull-request-number input, with the
same-repository guard. The payload adds `background` (the summary) and the rules from step 1, and the raw diff
stays in the payload: OCR reads it and treats the summary as navigation. Result: one updated comment with the
existing `<!-- ocr-review -->` marker, grouped by severity, showing commit, status, model, and degradation. The
daily and pre-release batches stay. A concurrency group per pull request cancels a stale run.
The dispatch address moves to `vars.OCR_DISPATCH_URL`; a missing value fails with a clear message. The owner
creates the variable first, because today no `OCR_DISPATCH_URL` secret exists and the hardcoded address is the
live value.

Done when: **[GH]** a ready-for-review event produces the comment; a deliberately weak test in a throwaway
branch is flagged under the rules from step 1 (this is the first real evidence the rubric works, and its
absence is a finding, not a pass). An `incomplete` result is never rendered as a pass.
Unverified: reviewer quality on real defects, until R5.
Rollback: revert; the batches keep working.

### 6. P2: `rg` and `fd` in the workspace

Change: the `prepare` job fetches upstream static builds of `rg` and `fd` by pinned version and checksum into
`.ci/bin` and includes it in the archive; `restore-workspace` appends it to `PATH`; the `apt` step is deleted
from `ci.yml`, `nightly.yml`, and `release.yml`. A missing binary then fails loudly instead of the tool
reporting itself unavailable under `NYAN_OFFLINE`.
Done when: **[GH]** the `Ensure fd and ripgrep` step is gone, `find`/`grep` tool tests pass, and the 30 hosted
jobs' 330 s of runner time (46 s worst) is replaced by the restore time. The measured per-run saving is
reported, not assumed.
Rollback: revert.

### 7. T1: Stryker incremental

Change: restore `.artifacts/stryker/protocol-incremental.json` before the mutation step (`main` saves, a pull
request only restores); the nightly run uses `--force` so a stale file cannot hide drift.
Done when: local `npm run test:mutation` is timed cold and warm first; **[GH]** `Deep verification` falls from
202 s on an unchanged `cbor/**` and still fails on a deliberately broken assertion. Verify that `--force` and
incremental restore behave as assumed before wiring them.
Rollback: revert.

**Status 2026-10-19: implemented, [GH] verification pending.** `deep-gate` restores the incremental file through
`actions/cache/restore` (`d4323d4d`, v4.2.2) keyed on the `cbor` sources and tests, saves it only on a push to
`main`, and the nightly runs `npm run test:mutation -- --force`.

Measured locally on this repository:

| run | time | score |
| --- | --- | --- |
| cold, no incremental file | 2 m 44 s | 74.60 |
| warm, unchanged tree | 6.7 s | 74.60 |
| a test file edited (assertion weakened) | 2 m 31 s | 74.60 |
| a whole test deleted | 2 m 38 s | 74.36, survivors 88 -> 89 |
| warm file, `-- --force` | 2 m 58 s | 74.60 |

So the incremental file does not hide drift: editing or deleting a test re-runs the mutants it covers, and the
survivor count moves. `--force` reaches Stryker through the npm script (unlike `--shard`, which npm swallows), so
the nightly really does run everything. `Deep verification` was 202 s of the pull-request critical path, of which
the mutation step was 175 s.

### 8. R4: gateway above CPA

Where it lives (U5). Recommendation: a separate repository, not a package here. It is owner infrastructure,
not a Nyan product, and a package would enter `layers.json`, the install lock, and the publish list.
Sub-steps, each with its own tests, because this is new code:
- 8.1 alias passthrough (`review.fast`, `review.deep`, `review.reflect`), authentication, health and readiness.
- 8.2 profile registry, capability probes, same-tier fallback, explicit result states (`complete`, `degraded`,
  `provider_unavailable`, `invalid_output`, `timeout`, `cancelled`, `queued_timeout`). A deep request never
  falls to a fast model.
- 8.3 a per-`review_run_id` ledger and outbox in SQLite; idempotent resume; completed stages not re-called.
- 8.4 per-profile bounded queues, deadlines, and cancellation of stale commits.
- 8.5 output validation against the schema from step 1, including the summary's coverage of changed files.
Deployed on `nyan-ops`, one instance, Docker or systemd restart, internal network only. Limits are set after a
load measurement taken from R2 and R3 traffic.
Done when: unit tests per sub-step; a kill-and-restart test that resumes without a second model call; an
outage test that returns `incomplete` and never "no findings".
Rollback: point PR-Agent and OCR back at CPA.

**Status 2026-10-19: started, in [blxzer77/nyan-gateway](https://github.com/blxzer77/nyan-gateway) (private).**
Where it lives is settled: its own repository, for the reason above and one more that turned out to matter more — a
separate process is a separate failure domain, so review traffic cannot spend nyan's own upstream key pool, cooldown or
concurrency budget. Putting the profile names in CPA's config would have been cheaper and coupled.

Implemented in its first commit (33 tests, `npm run check` green, CI green):

- 8.1 OpenAI-compatible surface: `POST /v1/chat/completions` where `model` is a profile, `GET /v1/models`,
  `GET /healthz`, bearer auth compared by digest. A caller changes a base URL and a model name, nothing else.
- 8.2 profile registry loaded from a versioned JSON file; a model that appears in two tiers is refused at load time,
  and the router only iterates the resolved profile's list, so same-tier-only fallback is structural.
- 8.4 concurrency is a per-profile in-flight counter with **no queue**: an overloaded tier answers `queued_timeout`
  immediately, per-attempt deadlines are enforced with an AbortController, and the lease is released on every path.
- 8.5 outcome vocabulary: `complete`, `unknown_profile`, `queued_timeout`, `timeout`, `provider_unavailable`,
  `invalid_output`, `stream_unsupported`, `upstream_rejected`. `complete` is the only success; an empty or unparsable
  reply is a failed review, never an empty result. A streamed request is refused rather than forwarded unvalidated.

Not done, and the reasons are decisions rather than gaps:

- **8.3 cross-run resume** is dropped. A review is a workflow run and its state is the run; the service holds no state,
  so a restart loses the in-flight request only.
- **Capability probes** are dropped: a candidate that cannot do tool calls is discovered when a caller asks for them.
- Only one candidate per profile today, so same-tier failover is proven by tests and not yet by configuration.

Open: deploy on `nyan-ops` with its own `GATEWAY_TOKEN` and its own upstream credentials — until that exists the
isolation is architectural, not real; and point steps 4 and 5 at the profiles, which is what makes them load-bearing.

### 9. R5: Langfuse and golden cases

Change: the gateway emits one trace per review run with the fields in decision 31 and writes evaluator scores
back, with an outbox for Langfuse downtime. In the repository, `.ci/ai-review/cases/`, `evaluators/`, and
`profiles/`. The first cases are seeded from defects this repository already fixed (the codemode worker-slot
flake, the `bash` full-output race, the remote-runtime probe timeout), by reintroducing each fix's inverse as a
diff, so the cases come from real history rather than invented bugs. A nightly job on `nyan-batch` runs the set
and writes scores.
Done when: a model or prompt change shows a before/after score in Langfuse; Langfuse stopped for a run loses no
review. Retention: metrics and cases kept, raw content 90 days.

### 10. R6: reflector

Change: a second profile on a different model or backend, an independent context, input limited to the finding,
its evidence, and the needed code, output `confirmed`, `rejected`, or `insufficient_evidence`, applied to high
and critical findings and to concurrency, recovery, security, and persistence findings.
Done when: on the golden set, the reflector lowers the false-positive rate and does not lower recall; both
numbers are reported.

### 11. P3: dynamic skip

Change: `if:` on `windows-smoke` and `mcp-conformance` from the existing `windows_required` and
`mcp_required`; the required list handed to `ci-gate.mjs` is generated from `policy.json` and the impact
outputs. `skipped` stays a failure for a job the impact outputs required. `policy.json` is cleaned: the stale
`advisory`, the 30 minute deep timeout, the unused target. This changes fail-closed logic, so it is the one
CI-script change for which a test is recommended even though the owner deferred CI-script tests; the owner
decides.
Done when: **[GH]** a documentation-only pull request skips both jobs and `Merge gate` is green; a change to
`packages/tui` runs Windows; a skipped required job turns the gate red.
Rollback: revert.

**Status 2026-10-19: implemented and merged in [PR #86](https://github.com/blxzer77/nyan/pull/86).** The required
list moved into `policy.json` as `required` plus `conditionalRequired` (job -> impact flag); `ci-gate.mjs` builds the
set per run, so a skipped job fails only when the classifier asked for it. `windows-smoke` and `mcp-conformance`
carry `if:` on their flags. `scripts/ci-gate.test.mjs` covers the fail-closed half; it was added despite the deferral
because this change alters fail-closed logic.

Evidence: the gate log of the PR's own run printed `Conditional checks required by the impact analysis:
windows-smoke, mcp-conformance, deep-gate` and `All 9 required CI checks passed.` — the job count came from the
impact outputs, not a literal. Three cases were also run against the committed policy directly: all flags false with
the three jobs skipped exits 0 with 6 required; `windows_required: true` with the job skipped exits 1 naming it; all
flags true with everything passing exits 0 with 9 required. `ci-impact.mjs` on a real docs-only commit reports
`risk: normal, fullSuite: false, windowsRequired: false, mcpRequired: false, deepRequired: false`.

Not observed yet: the skip on a real docs-only pull request, because this PR necessarily changes `.github/**`, which
the classifier treats as the full suite. That becomes visible on the first docs-only pull request after it merged.

### 12. P4: release preflight

Change: a `preflight` job in `release.yml` (with `actions: read`) that finds the `push` run of the CI workflow on
`main` for `GITHUB_SHA`, requires `Merge gate` success, waits a bounded time if it is still running, and
requires the tag commit to be an ancestor of `main`. `publish` needs it.
Done when: **[GH]** a dry run against a commit with a green push gate passes; against a commit without one it
fails with a named reason. A pull-request run's gate does not count.

### 13. T2: mutation scope and properties

Change: after T1 gives a warm baseline, measure mutant count and time for candidate directories (`durable`
harness and storage, the `agent` loop) before adding any; a pull request mutates changed lines only (Stryker's
`file:line-line` ranges) and the nightly runs the whole directory. Add `fast-check` properties to modules with
an invariant (serialization round trips, state transitions) in the order the owner picks.
Done when: for each added scope, a recorded time, a score, and a list of surviving mutants triaged into "add an
assertion" or "equivalent".

### 14. H: hygiene batch (independent, each its own change)

Shared composite action for the test steps; per-shard JUnit artifact and a nightly summary job;
`native.yml` on the shared toolchain with a presence-and-load check of every prebuild; scanner images pinned by
digest and Dependabot enabled; the weekly cache prune (14 days, 7 GB, `main` kept); `check-package-install.mjs`
run against the real packed tarballs in the release `pack` job; a faster Stage 0 hook. None changes the
decisions; all are low risk.

## Not in this plan

TDD; automatic changes to the working tree by the reviewer; Semgrep and SonarQube (open question 5); an
external monitoring service; high availability for the gateway; tests of the CI scripts (deferred by the owner,
except as stated in step 11).
