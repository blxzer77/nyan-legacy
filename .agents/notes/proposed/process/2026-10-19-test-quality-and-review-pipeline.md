# Agent Note: Test quality, automated review, and pipeline decisions

Status: proposed. Nothing in this note is implemented.

This note records the decisions taken in the 2026-10-19 discussion and the evidence behind the open
problems, so the work can be split into reviewable changes. It supersedes nothing: the impact classifier,
the fail-closed `Merge gate`, the risk tiers, the model-data snapshot, the workspace artifact, and
"publish exactly the verified bytes" stay as they are.

## Problem

Development runs at AI speed; human review has not scaled with it. The current gate answers "does it
build, type-check, lint and pass the suites", not "would a wrong change still pass". Tests are the durable
asset and the executable specification, so a test that passes against a broken implementation is worse
than no test.

Owner position: no TDD. Test-writing discipline is not enforced per change; the responsibility for
catching a weak or tautological test moves to an independent automated review, backed by a small number of
deterministic checks that are known to fail when the code is wrong.

Measured fact that bounds all of this: the only mutation testing in the repository covers
`packages/protocol/src/cbor/**` (`stryker.config.mjs`), one directory of one package out of twelve. There
is no evidence today that the suites of `agent`, `durable`, or `coding-agent` fail when their code is wrong.

## Decisions taken (owner, 2026-10-19)

### Pipeline

1. **Release is tag-only.** A real `npm publish` happens only for a `nyan-vX.Y.Z` tag push.
2. **A manual Release run is fixed to a dry run.** `workflow_dispatch` loses the `dry_run` switch.
3. **A release requires the same commit's `Merge gate` to have passed.** The check is a preflight, not a
   repeat of the suites. It must read the gate of a `push` to `main` for that SHA, because a pull-request
   run selects only the impacted packages while a push selects the full suite.
4. **Runner policy is "use both pools, parallelize everything".** GitHub-hosted runners by default;
   the self-hosted host takes the slowest test shards on the pull-request path. The slot budget stays at
   3 shards (a measured choice: six concurrent vitest pools on six cores gave load 17 to 26 and timing
   flakes). This reverses the earlier "keep pull requests off the shared host" rule.
5. **Self-hosted runners are labelled by purpose** (pull-request shards, nightly, OCR), with their user
   directories and credentials cleaned. The plaintext GitHub PAT recorded in `/root/.npmrc` on the host is
   rotated.
6. **Windows is Actions-only and dynamically skipped.** There is no local Windows machine; Windows
   coverage exists only as the `windows-latest` job, and it runs when the impact classifier selects it.
7. **`policy.json` becomes the single source of truth.** Required and advisory jobs, timeouts, and the
   dynamic conditions are generated from it or validated against it; the hardcoded copy in `ci.yml` goes.
8. **Caches stay and are pruned weekly:** entries not accessed for 14 days are deleted; if the total still
   exceeds 7 GB the oldest branch entries go first; `main`'s recent entries stay; the result is written to
   the job summary. (Priority is low, see the correction under "Gaps".)
9. **Docker scanner images (Gitleaks, Trivy, Syft) are pinned by digest**, and Dependabot is enabled.
10. **Test execution is one composite action** shared by `ci.yml`, `nightly.yml`, and `release.yml`; each
    workflow still owns its runner, matrix, and concurrency.
11. **Diagnostics stay GitHub-native:** a short-lived JUnit/log artifact per failed shard, a nightly
    summary job, no external monitoring, no automatic retry that hides a flake.
12. **Node 24 series only**, npm pinned at the CI value; `native.yml` uses the same toolchain and checks
    that every platform's prebuild is present and loads (not that it is byte-identical).
13. **No external actionlint.** `scripts/validate-workflows.mjs` stays the workflow check.
14. **Fuzz and mutation stay sequential in one job.** Fuzz is 1 s; splitting buys nothing, see "Gaps" for
    what does.
15. **The OCR dispatch address becomes a repository Variable** (`vars.OCR_DISPATCH_URL`); a missing value
    fails the run with a clear message. The token stays a Secret.
16. **Tests of the CI scripts are deferred** to a later change at the owner's request.

### Gate semantics

17. **Deterministic evidence blocks; AI findings do not, until reproduced.** Build, contract tests,
    integration tests, security scans, and mutation results block. An AI finding blocks only after a test,
    a rule, or a human reproduces it.
18. **The coding agent never reviews itself.** Review runs as a separate stack with no access to the
    implementation session, and it only reports; it never writes to the working tree. It may propose test
    patches, which the deterministic CI then judges.

### Automated review stack

19. **Context is the impact scope:** the diff, the public contracts it touches, callers and callees, the
    related tests, and the state, persistence, error, and concurrency paths. Not the whole repository,
    not the diff alone.
20. **Triggers:** PR-Agent `/describe` on every pull-request update; OCR deep review when a pull request is
    marked ready, on manual dispatch, and before a release; the daily review of `main` stays.
21. **The summary is a required intermediate artifact** with a schema (changed files, behavioural changes,
    public API changes, risk areas, affected tests, unknowns). The gateway checks that it covers the changed
    files. A failed summary is retried or switched inside the fast tier; if it still fails the run is
    `review_incomplete`. OCR always re-reads the raw diff: the summary is navigation, not evidence.
22. **One gateway above CPA.** The gateway owns task profiles (`review.fast`, `review.deep`,
    `review.reflect`), routing, health and circuit-breaking, capability probes (context, tool calls,
    structured output), timeout, cancellation, retry, fallback, and output validation. CPA stays the model
    transport, authentication, and protocol layer. PR-Agent, OCR, and the reflector never name a model.
23. **Fallback stays inside a tier.** A deep request never silently becomes a fast request. If every
    candidate fails the result is `incomplete` / `provider_unavailable`, which CI never reports as "no
    findings". The artifact records the profile, provider, model, and any degradation, never a credential.
24. **The reflector uses a different model or backend**, with a context independent of the main review. It
    receives only the finding, its evidence, and the code it needs, and returns `confirmed`, `rejected`, or
    `insufficient_evidence`. It runs for high and critical findings and for concurrency, recovery,
    security, and persistence findings.
25. **Queues are bounded per profile** with their own concurrency limit and a deadline that yields
    `queued_timeout`. The limits are set after a load measurement, not guessed.
26. **A newer commit cancels queued older reviews** and makes a best-effort cancel of running ones; a stale
    result is discarded, keyed by repository, pull request, commit, and stage.
27. **State is persisted per `review_run_id`.** A restart resumes from the outbox; completed stages are not
    called again; a duplicate webhook or a CI rerun does not duplicate a result.
28. **GitHub output:** one updatable PR-Agent comment and one updatable OCR comment (fixed markers),
    grouped by severity, showing commit, status, profile and model, and degradation. `incomplete` is never
    shown as a pass. Full results go to the run summary, an artifact, and Langfuse.
29. **The reviewer's access is read-only** to the repository, plus restricted validation commands in a
    sandbox. No write, commit, push, or unrelated network.
30. **Deployment:** the gateway is an independent long-running service on `nyan-ops` (the runner host
    keeps its CPU for the tests), reachable only on the internal network, one instance restarted by Docker or
    systemd, with a health endpoint and a local outbox. No high availability.
31. **Langfuse (owner's server) records traces and scores; the repository owns the dataset.** Golden review
    cases, expected findings, evaluator rules, and profile definitions are versioned in Git
    (`.ci/ai-review/`). Deterministic evaluators score a run (schema valid, changed-file coverage, finding
    has evidence, validation command result, mutant killed, latency, fallback used) and the gateway writes
    the scores back; an LLM judge may add a score but never decides a block. Langfuse being down must not
    lose a review: the gateway keeps an outbox and the result is also a CI artifact. Metrics and golden cases
    are kept without limit; raw prompts, diffs, and outputs for 90 days; nothing leaves the owner's server.
    The same Langfuse later evaluates Nyan itself.

## Corrections to the first pass of this note

The first version of this note overstated or got wrong several points. Each was re-checked against the
workflows and `gh api`.

- **"Release never runs Bun, MCP, supply-chain, Windows" is not a separate defect.** Decision 3 covers it:
  the `push` gate for the same SHA runs all of them. Counting it twice inflated the list.
- **"Cache growth is uncontrolled" was overstated.** GitHub already evicts an entry after 7 days without
  access and evicts oldest-first above 10 GB; 7.40 GB is under the limit. The count (76 entries, 25 refs, one
  key duplicated 7 times) comes from refs times OS/arch times lockfile hash, not from the number of jobs. The
  real risk is that pull-request caches evict `main`'s. A weekly prune is cheap and fine, but low priority.
- **"Hosted runners are about twice as slow" was an inference that the data does not support.** In run
  `39190190198`, `Node tests (coding-agent 4/4)` took 131 s on a hosted runner and 64 to 72 s for 1/4 to 3/4 on
  the self-hosted host. The hosted job's 131 s was `apt install ripgrep fd-find` 46 s, restore 8 s, setup 11 s,
  tests 60 s; the self-hosted test step took 47 s. The test step differs by about 1.3x; the rest is the apt
  step.
- **The tailnet address in `ocr-review.yml` was compared too strongly to the E3 rule.** That rule is about
  product defaults. This is an owner CI default in a public workflow, which publishes network topology but
  grants nothing. It is also the live value: no `OCR_DISPATCH_URL` secret exists, so the hardcoded default is
  what runs.
- **The run time for `39190190198` was recorded as 3.5 min; it was 4 min 34 s.** Recent pull-request and
  `main` runs take 183 to 234 s.
- **"Nothing verifies that the packages install" needs a precise form.** `coding-agent-smoke.test.mjs` runs
  inside the root script tests, but on a synthetic fixture of fake packages: it tests the smoke logic, not the
  real packed tarballs. `check-package-install.mjs` installs the real artifacts and is in no workflow and not
  in `npm run check`.

## Gaps found (verified 2026-10-19)

1. **`windows_required` and `mcp_required` are computed and never read.** `ci.yml` exports both from
   `policy`; no job has a matching `if:`. The skip also needs `CI_GATE_REQUIRED` to drop the name, because
   `ci-gate.mjs` fails a required job whose result is `skipped`.
2. **The release job can be run on any branch, and its environment has no protection.** The `npm`
   environment has no protection rules and no deployment-branch policy, and `NPM_TOKEN` is a repository
   secret, not an environment secret. A tag check written in the workflow can be edited on a branch by
   whoever can push a branch and dispatch, which includes an agent holding a token. Decision 1 therefore
   needs a repository setting as well as a workflow edit: restrict the `npm` environment to tags matching
   `nyan-v*` and move `NPM_TOKEN` into it.
3. **`Deep verification` is the critical path, and its incremental mode does nothing in CI.** It took 202 s
   in run `39190190198`, longer than any test shard; the mutation step is 175 s of it. `stryker.config.mjs`
   sets `incremental: true` with its file in `.artifacts/stryker/`, which a fresh runner never restores, so
   every run mutates the whole of `cbor/**`. Persisting that file is the cheapest path to the incremental
   mutation the handover asks for. A nightly run must still ignore it, or a stale file hides drift. The size
   of the gain is not measured.
4. **`rg` and `fd` are installed by `apt` in 30 hosted jobs per run**, 330 s of runner time in total, 46 s at
   worst, and that worst case sits on the hosted straggler shard. An earlier note proposed shipping them in the
   workspace archive (`.ci/bin`) and never did. The tools are looked up on `PATH`
   (`packages/coding-agent/src/utils/tools-manager.ts`), so an archive entry plus a `PATH` line is enough.
5. **The 3-slot budget splits the four `coding-agent` shards 3 and 1.** The fourth runs hosted. Once the apt
   step is gone the difference is about 13 s of test time, so fix item 4 first and re-measure before touching
   the shard count.
6. **The nightly runs during working hours.** The cron is `17 2 * * *`, UTC. The owner's commits carry
   `+08:00`, so 02:17 UTC is 10:17 local, and the last observed run started at 07:17 UTC, 15:17 local. It holds
   up to five self-hosted slots then, which collides with decision 4. The OCR batch (18:30 UTC, 02:30 local) is
   fine. Scheduled runs start 2 to 9 hours late, which the repository cannot control.
7. **All six self-hosted runners carry identical labels**, so a pull-request shard cannot be kept off a host
   a nightly or an OCR batch is using. The one recorded timing flake (`tools-images`, a 100 ms event-loop bound
   measured at 153 ms) came from a nightly and a release sharing the host. Decision 5 is a prerequisite of
   decision 4, not an extra.
8. **`policy.json` has drifted.** `required` names eight job keys while branch protection requires only
   `Merge gate`; `advisory` names `ai-review`, which is not a job; `deepGateTimeoutMinutes` is 30 and the job
   times out at 20.
9. **The mutation scope is one directory.** See the measured fact under "Problem". This is the gap that
   matters most for the stated goal, and none of the pipeline changes above touches it.
10. **`Merge gate` is sometimes red on `main`.** Two of the last eight `main` pushes failed: in one,
    `durable-resources.test.ts` under Bun (`Could not resolve model: Unknown provider "cliproxyapi"`); the
    other failure was a Node shard whose cause was not read. They come from in-flight product work, not from
    the pipeline, and they are why decision 3 checks the exact SHA.
11. **Free coverage is unused:** `dependabot_security_updates`, secret-scanning non-provider patterns, and
    validity checks are disabled. `PACKAGE_WEIGHT` in `ci-shards.mjs` ranks only two packages.
12. **Only 2 of 40 jobs in run `39190190198` waited more than 5 s for a runner** (37 s and 62 s). The
    20-job hosted limit is not today's bottleneck.
13. **The review stack has no repository-side configuration at all.** There is no `.ocr/` directory, no
    `.pr_agent.toml`, and no rubric: the OCR payload in `ocr-review.yml` carries `repo`, `sha`, `base_sha`,
    `diff`, `model`, `effort`, `timeout_minutes`, and `reasoning_effort`, with no `background` and no rules. OCR
    therefore reviews with generic judgement and knows nothing about this project's test standards. This is the
    first thing the owner's goal needs, and it needs no infrastructure.
14. **`AGENTS.md` carries no test-quality rule.** A search of `AGENTS.md`, the four package files, and the
    skills finds nothing on mocking, black-box contracts, tautological assertions, boundary cases, or mutation.
    The only test rules are "run the test you changed" and "skip only when the platform cannot run it". The
    handover's "blind contract tests, no internal mocks" is not written down anywhere an agent or a reviewer
    reads.
15. **A pull-request trigger on the self-hosted host needs a same-repository guard that does not exist yet.**
    `ocr-review.yml` dropped its pull-request trigger on purpose ("no pull request can reach this runner").
    Decisions 20 and 28 bring one back for PR-Agent and for OCR on `ready_for_review`. The repository is public,
    the fork-approval policy is only `first_time_contributors`, and the host's users hold `NOPASSWD` sudo and
    the docker group. A fork pull request must therefore never be scheduled there: the job needs
    `github.event.pull_request.head.repo.full_name == github.repository`, `pull-requests: write` on that job
    only, and no checkout of fork code.
16. **Stage 0 is not fast.** `.husky/pre-commit` runs the whole `npm run check` (lint, typecheck, six
    consistency checks, browser smoke). An earlier measurement put the single `check` at 30 s on the self-hosted
    host and 38 s hosted. The handover's target is under 3 s. Husky is already the hook manager; whether to
    switch to Lefthook is not decided here.

## Open questions

Not decided. Each needs an owner answer before implementation.

1. **Mutation testing scope** (the largest). Which directories after `cbor/**` (candidates: `durable`
   harness and storage, the `agent` loop), changed lines only on a pull request (Stryker accepts
   `file:line-line` ranges) or whole directories with the persisted incremental file, a score floor, and
   whether a surviving mutant blocks or reports.
2. **Property tests.** Which modules get `fast-check` properties first; the repository already has
   `fast-check` and a protocol fuzz harness.
3. **Unit and integration separation.** Whether a physical split is needed at all. The durable suites use
   SQLite files and temporary directories, so the handover's Testcontainers singleton has no equivalent here.
4. **Where the summary stage runs.** A separate PR-Agent service, or a summary step added to the existing OCR
   dispatch.
5. **Semgrep and SonarQube.** Whether either is adopted, given Gitleaks, Trivy, `npm audit`, the protocol
   fuzz harness, and Stryker already run.
6. **Where review results live while repository Issues are disabled.** Today it is the run summary and a
   30-day artifact: no history, and no reference from a commit.
7. **Where the gateway's own tests and golden cases run**, since the gateway is outside the repository's CI.
8. **Whether the OCR dispatch service on `nyan-ops` accepts a `background` field and project rules.** The
   repository cannot show this; the service's own interface has to be read. Decision 21 depends on it.
9. **The format and location OCR reads project rules from.** Not inspectable from this repository.

## Verification

Nothing here has been implemented. The observations come from `git show HEAD:.github/workflows/*.yml`,
`gh api repos/blxzer77/nyan/{environments,actions/secrets,actions/cache/usage,actions/runners,branches/main/protection}`,
`gh api repos/blxzer77/nyan --jq .security_and_analysis`, the job and step timings of run `39190190198`, the
run durations of the last eight `main` and pull-request runs, and the failed-job log of run `39205152453`.
