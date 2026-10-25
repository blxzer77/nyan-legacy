# Agent Note: Hosted parallel CI/CD for Nyan

Status: proposed

Progress: implemented and merged for the pull-request path (3.48 min), nightly (9.1 min, self-hosted by decision), release (5.7 min, all hosted), the daily scan and batched OCR. OCR is recorded in the [implemented OCR note](../../implemented/process/2026-10-18-ocr-review-cadence.md). Still open: runner role labels, removing the six `nyan` runners, and rotating a token.

Supersedes the runner-placement and job-shape bullets of [Nyan quality workflow architecture](2026-10-17-quality-workflow.md). The impact classifier, fail-closed gate, risk tiers, model-data snapshot, artifact reuse, and "publish exactly the verified bytes" decisions stay.

## Problem

The quality gate in `ci.yml` takes 22 minutes on a full pull request while 17 of its 25 test jobs do under 15 seconds of actual work, and the same run leaves the only CI host at roughly twice its CPU capacity. Measured on `origin/main` at `6060afa73`:

- Run `39028721085` (full PR, success) took 22.1 minutes: `policy` 24 s, `prepare` 38 s, the node/bun matrices 13.3 minutes of wall clock, `deep-gate` 7.4 minutes, `gate` 12 s.
- `.github/quality/policy.json` sets `pullRequest.targetP95Minutes: 10`. The gate misses its own target by more than 2x.
- Before the matrix split, the same change took 6.4 minutes (run `39029129651`, one vendor job per platform). Splitting into 25 matrix jobs made the pipeline 3.5x slower.
- `nyan-ci` (6 vCPU, 23 GiB) reported `load average: 3.81, 8.92, 11.52` for 1/5/15 minutes while six runner services were busy: 1.9x oversubscribed. Each job starts `vitest` with `maxWorkers = cpus - 1 = 5`, so six concurrent jobs run about 30 worker threads on six cores.

Three independent causes, in order of size:

1. **`max-parallel: 2` on two separate matrices.** 28 test jobs compete for 4 slots, so the last jobs queue for 12.7 minutes. The 6 self-hosted runners are the second limit: even without `max-parallel`, only 6 jobs can run.
2. **`deep-gate` re-runs work that already passed.** Of its 445 s, 182 s is `npm run test:scripts` plus five package suites and 146 s is the same five packages under Bun, both already covered by `node-tests` / `bun-tests` in the same run. Only the 1 s fuzz harness and the 93 s mutation run are unique. It also carries `concurrency: nyan-deep-gate / cancel-in-progress: false`, so two open PRs serialize.
3. **Per-job fixed cost dominates small jobs.** `checkout` 2 s + `Setup Nyan` (`npm ci`, 15 s) + artifact download 3 s is 20 s for a job whose test step is 1-14 s. 17 of 25 test jobs are in that class.

A fourth cause showed up only once the first two were fixed, and it was larger than either: **the
self-hosted pool is shared across every open pull request.** With two other pull requests open from
other sessions, the first hybrid run (figure: run `39038704551`) had `supply-chain` wait 194 s for a
slot, `prepare` wait 260 s behind it, and `deep-gate` wait again — three self-hosted jobs running
strictly one after another on six slots. `prepare` heads the critical path, so that queue alone cost
more than the whole test phase it feeds. A per-core speed advantage does not survive a five-minute
queue; the pool is most contended exactly when feedback is most wanted.

Secondary costs found while measuring:

- **Cache storage is 72% consumed and thrashing.** `gh api repos/blxzer77/nyan/actions/cache/usage` reports `active_caches_size_in_bytes: 7173534654` (7.17 GB) across 74 entries against the 10 GB limit. Almost all of it is `node-cache-<OS>-<arch>-npm-<lockfile sha>`, one entry per pull request because `actions/cache` is branch-scoped and every PR writes its own.
- **`npm ci` on an already-populated cache is 9.1 s, not 20 s.** Measured on `nyan-ci` after `git clone --depth 1`: `added 421 packages in 9s`. The 20 s in CI is cache restore plus network, not extraction.
- **The install output is small enough to ship.** `node_modules` is 438 MB / 23 233 files on Linux; `tar | zstd -3` gives 100 MB and extraction takes 0.79 s. A `node_modules` + `dist` artifact is therefore cheaper than a per-job install.
- **The apt step is probably unnecessary.** `ci.yml` installs `libcairo2-dev libpango1.0-dev libjpeg-dev libgif-dev librsvg2-dev fd-find ripgrep` in five jobs. The only `canvas` import in the repository is `packages/ai/scripts/generate-test-image.ts`, a one-off generator; `canvas` is otherwise a `devDependencies` entry of `packages/ai` with no importer at test time, and `npm ci` runs with `--ignore-scripts`. Only `fd`/`rg` may matter, and `packages/coding-agent/src/utils/tools-manager.ts` looks both up on `PATH`.
- **`npm run check` never reads `dist`.** `tsconfig.json` maps every `@ny4n/*` specifier to `packages/*/src`, and `check-entry-graphs.mjs`, `check-architecture-layers.mjs` and `check-ts-relative-imports.mjs` either derive source paths from `exports` or skip `dist`. The `static` job still waits for `prepare` to finish building, which is pure serialization.
- **GitHub-hosted runners are free for this repository.** `blxzer77/nyan` is public (`gh repo view --json visibility`), and every `/actions/runs/<id>/timing` response reports `total_ms: 0` for UBUNTU, WINDOWS and MACOS, including the 8.7-minute release run `39029131011`. Hosted is about 25% slower per core than `nyan-ci` (`npm ci` 20 s vs 9 s, `npm run check` 38 s vs 30 s, `build:offline` 17 s vs 12 s) but it is dedicated and effectively unbounded, which is the trade this design takes.

## Proposal

Move ordinary CI to GitHub-hosted runners, stop paying per-job install cost, and make the longest single job the critical path instead of the job count.

### Non-negotiables

- A release still publishes the exact bytes that `verify` built and tested; no job rebuilds after packing.
- Fail-closed impact analysis, the model-data snapshot hash, SHA-pinned actions, and the `Merge gate` aggregator keep working unchanged.
- `policy.json`'s `impactAnalysisFailClosed`, `criticalPaths`, and deep-testing thresholds are untouched.
- No job gains a credential it does not already have. Moving to hosted removes the fork-PR trust split, so `needs.policy.outputs.fork_pr` guards and the `head.repo.full_name` `runs-on` ternaries are deleted rather than ported.

### Runner placement

Implemented for the pull-request path, with one correction to the original design.

| Work | Today | Implemented | Reason |
| --- | --- | --- | --- |
| policy, static, prepare, matrices, deep, gate, supply-chain, mcp | `[self-hosted, Linux, X64, repo-nyan]` | `ubuntu-latest` | measured 20-job limit; per-run dedicated runners |
| windows-smoke | `windows-latest` | unchanged | already hosted |
| native `linux-x64`, release prepare / check / tests / pack / binaries / smoke linux-x64 | self-hosted | `ubuntu-latest` | a release dry run took 9.9 min on the shared host and 5.7 min hosted; see Phase 3 results |
| npm-audit (daily scan) | self-hosted | `ubuntu-latest` | the same scanners already run hosted in the pull-request path |
| nightly | self-hosted | self-hosted (decided, not a pull-request path) | matrix widths 2 + 2 + 1 so six vitest pools do not share six cores |
| OCR review | self-hosted on every PR push | self-hosted in daily and release batches | the dispatch service on `nyan-ops` is tailnet-only; `gpt-6.1-sol` at service maximum `high`, advisory |

**Correction: nothing urgent runs on the self-hosted pool.** The first attempt kept `prepare`,
`supply-chain` and `deep-gate` on self-hosted on the theory that warm npm cache, local Docker images
and the Trivy database made them cheaper there. Measured on run `39038704551`, with two other pull
requests open from other sessions:

- `supply-chain` waited 194 s for a slot, then `prepare` waited 260 s and started two seconds after
  `supply-chain` finished. `deep-gate` then waited again. Three self-hosted jobs ran strictly one
  after another on the six-slot pool.
- `prepare` is the head of the critical path, so its 260 s queue cost more than the entire test phase
  it feeds. The run took 10.0 min, and every second above the test phase was that queue.

The pool is six slots on a 6-vCPU host shared by every open pull request, so it is most contended
exactly when feedback matters most. A per-core speed advantage does not survive a five-minute queue.
Every pull-request job therefore runs on `ubuntu-latest`, and the self-hosted host takes the heavy,
non-urgent work — nightly suites and the release build — where queueing cannot delay a pull request.

Consequence for the fork-PR split: with no pull-request job on self-hosted, the `fork_pr` guards and
the `head.repo.full_name` runner ternaries protect nothing, so both were deleted.

`nyan-ci` keeps its six runner services; they are the target for the nightly and release work.

### Prewarmed workspace instead of per-job install

`prepare` becomes the single install+build point and publishes one artifact:

```yaml
prepare:
  name: Prepare workspace
  runs-on: ubuntu-latest
  timeout-minutes: 15
  steps:
    - uses: actions/checkout@<sha>
    - uses: ./.github/actions/setup-nyan     # setup-node (no cache) + npm ci + model-data restore
      with: { node-version: '24', bun: 'false', model-data: 'true' }
    - run: npm run build:offline
    - name: Pack workspace
      run: |
        tar -C . -cf - node_modules packages/*/dist .ci/model-data.lock.json \
          | zstd -3 -q -o workspace-linux.tar.zst
    - uses: actions/upload-artifact@<sha>
      with:
        name: nyan-workspace-${{ github.sha }}
        path: workspace-linux.tar.zst
        if-no-files-found: error
        retention-days: 3
```

Every Linux job then replaces setup+install+dist-download (20 s) with:

```yaml
    - uses: actions/checkout@<sha>
    - uses: actions/setup-node@<sha>
      with: { node-version: '24' }            # toolcache link only, no `cache: npm`
    - uses: actions/download-artifact@<sha>
      with: { name: nyan-workspace-${{ github.sha }} }
    - run: tar -xf workspace-linux.tar.zst
```

Measured shape: 100 MB zstd archive, 23 233 files, 52 workspace symlinks (`node_modules/@ny4n/* -> ../../packages/*`) preserved by tar, 0.79 s extraction on `nyan-ci`. A zstd archive rather than a raw upload matters: upload-artifact's own compression is not worth re-compressing 438 MB of already-compressed content.

Bun jobs add `oven-sh/setup-bun` (5 s) and keep the same artifact. Windows keeps its own `npm ci` because `node_modules` is platform-specific there.

`.ci/bin/{fd,rg}` may be added to the packed workspace by `prepare` so no job needs `apt-get`. This removes the five `Install Linux build dependencies` steps. Verify before deleting them: one `ubuntu-latest` run must show `build:offline`, `npm run check`, `coding-agent` tests, and `test:mcp-conformance` passing without the package install.

### Cache policy

With the workspace artifact, `setup-node cache: npm` is only needed in `prepare`. Leaving it on 25 jobs is what produced 74 entries and 7.17 GB. Two changes:

- `prepare` keeps `cache: npm` keyed by `package-lock.json`; `cache-dependency-path: package-lock.json`.
- PR runs restore only, main runs save. Either use `actions/cache/restore` + a save step guarded by `if: github.event_name == 'push'`, or set `setup-nyan`'s `install` input so that PRs run `npm ci --prefer-offline` against a restore-only cache. The point is that a pull request never writes a 100 MB npm cache that is scoped to a ref nobody will reuse.

Target: 2-4 cache entries per lockfile (Linux/Windows toolchain plus one npm cache) instead of 74.

### Job graph

```
Wave 0  (t = 0)
  prepare        ubuntu-latest  ~70 s   install + build:offline + pack workspace
  static         ubuntu-latest  ~55 s   npm run check (needs node_modules only, not dist)
  policy         ubuntu-latest  ~15 s   validate-workflows + ci-impact (no npm ci)
  supply-chain   ubuntu-latest  ~70 s   gitleaks + npm audit + signatures + trivy
  mcp            ubuntu-latest  ~60 s   conformance (needs prepare)
  windows        windows-latest ~110 s  own install + build + check

Wave 1  (needs prepare)
  node matrix    ubuntu-latest  9 jobs, longest ~70 s
  bun matrix     ubuntu-latest  5 jobs, longest ~60 s
  deep extras    ubuntu-latest  ~95 s   fuzz FUZZ_RUNS=5000 + mutation only

Wave 2
  gate           ubuntu-latest  ~15 s
```

Critical path: 70 s prepare + 70 s longest shard + 15 s gate ≈ **155 s**, with a p95 target of 4 minutes including hosted cold start. Against the 22.1-minute baseline that is a 7x reduction, and it puts `policy.json`'s 10-minute target back within reach.

`policy` in wave 0 is cheap because it stops running `npm ci` and the `docker run rhysd/actionlint` step: `npm run check:workflows` already validates that file, and actionlint is a lint of the same file. Keep `validate-workflows.mjs` and drop the container.

### Test matrices

Sharding by file count is not enough. Measured per-file cost shows the 4 slowest `coding-agent` files and the 7 slowest `durable` files hold most of the work:

| package | files | summed file duration | slowest files |
| --- | --- | --- | --- |
| coding-agent | 317 | 903 s | `git-update` 72 s, `experimental-presentation-facets` 60 s, `model-registry` 55 s, `tools` 42 s |
| durable | 64 | 748 s (one pass) | `tools-read-differential` 103 s, `jsonl-storage` 98 s, `codemode` 91 s, `tools-images` 80 s, `env-node-conformance` 74 s, `harness-scheduler-retention` 69 s, `provider-session-cache-e2e` 55 s |

A single 72 s file caps any shard that contains it, which is why `--shard` saturates: full 184 s, `1/2` 109 s, `1/4` 84.8 s, `1/8` 79 s on a 16-core Windows box. Round-robin cannot separate those files.

Target shape, with `max-parallel` chosen so the whole run stays inside the concurrency budget:

```yaml
  node-tests:
    name: Node tests (${{ matrix.group }})
    needs: prepare
    runs-on: ubuntu-latest
    timeout-minutes: 15
    strategy:
      fail-fast: false
      max-parallel: 6
      matrix:
        include:
          - { group: coding-agent-1, cmd: 'npx vitest --run test/git-update.test.ts' }
          - { group: coding-agent-2, cmd: 'npx vitest --run --shard=1/3 ...' }
          # ... 4 groups total for coding-agent, weights balanced from the committed profile
          - { group: durable-1,      cmd: 'npx vitest --run --shard=1/3' }
          - { group: small,          cmd: 'for p in protocol client server mcp telemetry chord tui codemode agent; do npm -w packages/$p test; done' }
```

Two implementation details:

- **`npm test -- --shard=...` silently ignores the flag.** `npm --workspace packages/coding-agent test -- --shard=1/2` ran all 317 files in the probe. Shards must invoke `npx vitest --run --shard=i/n` directly, or a new script must be added, for example `"test:shard": "vitest --run"` in each package, called as `npm -w packages/<p> run test:shard -- --shard=i/n`.
- **`packages/durable` runs its suite twice.** `test` is `vitest --run && node scripts/test-without-document-retention.mjs`, and the retention script forwards no arguments, so a shard flag reaches only the first pass. `scripts/test-without-document-retention.mjs` must forward `process.argv.slice(2)` to `vitest`, and the shard job runs both passes for its own slice.

The `small` group exists because nine packages cost 1-14 s each. Running them sequentially inside one job costs 20 s of setup instead of nine separate setups, and it keeps the job count inside the limit. This is deliberately not a full aggregation: a probe that ran eight packages as one vitest `projects` list (51 files, 908 tests) finished in 47 s but failed 5 tests that pass in isolation, including `packages/codemode/test/sandbox.test.ts` (output-limit timing) and `chord`'s state-fuzz timeout, because the projects competed for CPU. Sequential `npm -w ... test` keeps each package's own worker pool and avoids that.

### Deep verification

`deep-gate` keeps only what the sharded matrices do not already cover:

```yaml
  deep-extras:
    name: Deep verification
    needs: prepare
    if: needs.policy.outputs.deep_required == 'true'
    runs-on: ubuntu-latest
    timeout-minutes: 20
    steps:   # download workspace, then:
      - run: FUZZ_RUNS=5000 FUZZ_SEED=20261017 npm run test:fuzz:protocol
      - run: npm run test:mutation
```

Removing `concurrency: nyan-deep-gate` lets two risky PRs verify at the same time. Removing the duplicated suites drops 328 s of the old 445 s.

### Nightly

`nightly.yml` runs on the self-hosted host. `prepare` builds once and expands every package into shards; a Node
matrix and a Bun matrix (width 2 each) run the full suites from the restored workspace, and `deep` runs the
protocol fuzz (10,000 runs) and the mutation test in parallel with them. `recovery` is gone: its package list
repeated `node-full`. Measured at 9.1 min. See Phase 3 results for why it stays self-hosted and for what is still
duplicated with `ci.yml`.

### Release

`release.yml` runs entirely on `ubuntu-latest`: `prepare` (build, pack the workspace, expand the shards), `check`
and a Node shard matrix run in parallel, `pack` runs only after both passed and packs the tarballs with
`--skip-build` from the restored workspace, `binaries` compiles from that same workspace without rebuilding,
four smoke tests run on matching runners, and `publish` uploads the packed artifacts after checking that the
manifest's commit equals `GITHUB_SHA` and that it was not dirty. Nothing after `prepare` rebuilds, so the bytes
that were tested are the bytes that are published. Measured at 5.7 min for a dry run.

### Concurrency budget

The plan-level concurrent-job limit was measured rather than assumed. `gh api
/user/settings/billing/actions` and the user billing endpoints return 404, so a throwaway probe was the
only way to learn it: 40 `ubuntu-latest` jobs with a 90 s hold, `max-parallel: 40`, peak overlap read
from the job intervals. Result: **20 concurrent jobs** (probe `39038007064`, peak at
`2026-10-17T13:47:37Z`). Recorded as `pullRequest.concurrencyBudget` in `.github/quality/policy.json`.

Matrix widths are caps, not reservations: the Node matrix may use 12 and the Bun matrix 8, and together with
`static`, `mcp`, `windows`, `deep` and `gate` the graph asks for more than 20 at once. GitHub queues the excess
instead of failing it, so the account limit does the scheduling. Measured: peak 20 live jobs in run
`39049211499` (3.48 min). Widths of 8 and 4 left the Bun matrix running in four waves and the gate waiting for the
last one (4.4 min), which is why they were raised.

### Required checks

Set on `main` after #39 merged: `Merge gate` alone, strict, applied to admins, no force push, no deletion.

`policy.json` and `.github/quality/policy.json`'s `pullRequest.required` list eight job names, and branch protection must name each one. Every matrix rename (`Node tests (ai)` → `Node tests (small)`) therefore breaks a required check and leaves a PR permanently pending. Two changes:

1. Branch protection requires exactly **`Merge gate`** (plus `Policy and impact` if a second independent signal is wanted). `scripts/ci-gate.mjs` already fails when any required job is not `success`, so the aggregate is the contract and the matrix names become an implementation detail.
2. `ci-gate.mjs` keeps treating `skipped` as a failure for a required job. `deep-extras` stays conditionally required through `deep_required`, as today. Add `advisory: []` handling only if an advisory job is introduced.

Also confirm `merge_group` still selects the gate: the gate currently derives everything from `needs.*.result`, so it needs an `always()` and no `if:` that can skip it.

### Failure reporting

Per-package jobs today localise a failure by name. Shards lose that. Compensate with:

- `--reporter=dot,github-actions` (already set in most configs) so annotations name the file and test.
- `--reporter=junit --outputFile=junit.xml` on shard jobs plus `actions/upload-artifact` with `if: always()` and `retention-days: 3`, so a merged job can still be attributed.

## Alternatives considered

- **Keep self-hosted and only remove `max-parallel`.** Cheapest change, but 6 runners cap the run at 6 concurrent jobs, so the two-shard-limit becomes a six-runner limit and 28 jobs still take two waves. `nyan-ci` stays at ~2x CPU and the 74-entry cache problem stays.
- **Raise the runner count on `nyan-ci`.** The host is 6 vCPU/23 GiB. More runner services increase oversubscription; it cannot be fixed by concurrency settings alone.
- **Add `nyan-temp` (2 vCPU, 7 GiB, idle) as a runner.** Useful only for native arm64-style builds; 2 vCPU cannot take a `coding-agent` shard without becoming the new critical path.
- **Aggregate all packages into one vitest `projects` run.** Measured: 47 s for eight packages and 908 tests, but 5 tests fail from CPU contention that pass in isolation. Also collapses per-package failure isolation. Rejected in favour of sequential small-package grouping plus weighted shards.
- **Bake a `ghcr.io/blxzer77/nyan-ci` container image with node, bun, and system libraries.** Removes `setup-node` (5 s) and apt entirely, but adds an image build/publish/rotation workflow and a registry credential for a 5 s gain over the workspace artifact. Reconsider only if the apt step turns out to be required.
- **Use `actions/cache` for `node_modules` instead of an artifact.** Same effect, but cache entries are branch-scoped and capped at 10 GB, which is the failure already observed at 7.17 GB / 74 entries. Artifacts are run-scoped and expire.
- **Speed up the slow tests instead of sharding around them.** The right long-term fix: 4 `coding-agent` files and 7 `durable` files are the majority of both suites' CPU. Out of scope here because it changes product test behaviour; recorded as a follow-up so the shard weights are not mistaken for the real fix.
- **Keep a self-hosted fallback lane in every workflow.** Doubles every job definition and every required-check name for a rollback that a one-line `runs-on` change already provides.
- **Move to a larger paid runner.** Unnecessary: the workload fits in 16-20 free concurrent hosted jobs, and paid minutes are the only thing this design avoids spending.

## Consequences

- Ordinary PR feedback drops from 22 minutes to about 3, p95 4, and no CI work runs on `nyan-ci`. The 6-vCPU host stops being a single point of failure for the whole repository.
- The pipeline loses warm-runner locality: hosted per-core throughput is about 25% behind `nyan-ci`, and every job pays a 3-5 s checkout and toolcache link. That is repaid by dedicated cores and by removing 17 jobs' worth of install.
- The workspace artifact makes test jobs depend on `prepare` having succeeded. A `prepare` failure now blocks every test job at once instead of failing one vendor job; the fail-closed gate reports it as one reason.
- Weight-based shard groups are a maintained list. When a suite's slowest files change, the balance degrades silently until someone re-measures. Mitigation: keep the profile generator and re-measure quarterly, or accept `--shard` round-robin as the fallback when weights are older than the last 30 commits.
- Deleting the fork-PR guards removes code paths that no longer have a purpose. It also means a fork PR now runs the same jobs as an internal one, which is only safe because no self-hosted runner is involved. This must not be reverted while self-hosted runners are still registered.
- `merge_group` runs become cheap enough to keep enabled without slowing the queue.
- A run asks for more concurrent jobs than the account allows and relies on queueing. If the real limit drops, wall clock degrades by a wave rather than failing, but two pull requests at once now compete for the same 20 slots.

## Verification

### Measured for this note before implementation

- `gh run list --workflow=ci.yml` plus `/actions/runs/<id>/jobs` for runs `39028721085`, `39029129651`, `39028540637`, `39030387971`, `39030398500`: per-job and per-step durations, 22.1 min full-PR baseline, 6.4 min pre-matrix baseline, 12.7 min worst queue.
- `/actions/runs/<id>/timing` for `39029131011`, `38660834853`, `39029129651`, `39028540637`: `total_ms: 0` for UBUNTU/WINDOWS/MACOS. `gh repo view --json visibility` reports `PUBLIC`.
- `ssh nyan-ci`: `nproc 6`, 23 GiB, `load average: 3.81, 8.92, 11.52`, six `actions.runner.*` services, `/root/.npm` 151 MB.
- `git clone --depth 1` on `nyan-ci`: `npm ci --ignore-scripts` 9.1 s, `node_modules` 438 MB / 23 233 files, `tar | zstd -3` 100 MB, extraction 0.79 s, 52 workspace symlinks preserved.
- `packages/coding-agent`: full 184 s, `--shard=1/2` 109 s, `1/4` 84.8 s, `1/8` 79 s. `packages/durable`: `1/2` 37 s, `1/3` 24 s. JSON reporter profiles: coding-agent 317 files / 903 s, durable 64 files / 748 s.
- `npm test -- --shard=1/20` in `packages/coding-agent` ran all 317 files, confirming the flag is swallowed by the npm wrapper.
- Eight-package `projects` probe: 51 files, 908 tests, 47 s, 5 failures that pass when run alone (`packages/codemode/test/sandbox.test.ts`, `packages/chord/test/state-fuzz.test.ts` and others).
- `npm run check` sub-step timings on a 16-core Windows box: `typecheck` 26.2 s, `check:runtime-deps` 25.7 s, `lint` 16.1 s, `check:ts-imports` 13.6 s, remaining checks ≤ 3.4 s each.
- Source inspection for the apt question: only `packages/ai/scripts/generate-test-image.ts` imports `canvas`; `packages/coding-agent/src/utils/tools-manager.ts` resolves `fd`/`fdfind` and `rg` from `PATH`.

### Measured during implementation

**Concurrent-job limit = 20 on GitHub-hosted.** Probe run `39038007064` (branch `probe/concurrency`,
deleted after measuring): 40 `ubuntu-latest` jobs with `max-parallel: 40` and a 90 s hold. Peak overlap
was 20 at `2026-10-17T13:47:37Z`, computed from the job intervals, not from a single sample. All 40
jobs started. Recorded as `pullRequest.concurrencyBudget.githubHosted` in `policy.json`.

The note's working assumption of 16 was therefore conservative. `max-parallel` is 8 for Node and 4 for
Bun; peak live jobs in the target graph is 18 (8 + 4 + static + mcp + windows + gate), inside the
limit with two slots spare.

**Pull-request pipeline: 22.1 min -> 4.8 min.** Run `39039960657` on `ci/hosted-parallel`, full suite,
all 33 jobs started, wall clock 4.8 min (`14:15:23Z` -> `14:20:11Z`). The first attempt at the split
(run `39038704551`, hybrid placement) took 10.0 min and is the evidence for the correction above.

| job | seconds | wait before start |
| --- | --- | --- |
| Node tests (coding-agent) | 182 | 65 |
| Deep verification | 173 | 65 |
| Node tests (durable) | 141 | 65 |
| Bun tests (durable) | 138 | 116 |
| Bun tests (coding-agent) | 136 | 109 |
| Windows build and check | 121 | 65 |
| Static checks | 89 | 15 |
| Prepare build artifact | 54 | 0 |
| Policy and impact | 12 | 0 |

`prepare` no longer queues: it starts immediately and finishes in 54 s, so the matrices begin at +65 s
instead of +317 s. `static` starts 3 s after `policy` and runs concurrently with `prepare`, confirming
the `dist`-independence claim. Sum of job seconds is 2157 s, so the run is bounded by the two longest
shards rather than by job count.

One job failed on the first attempt of `39039960657` — `Bun tests (coding-agent)`,
`agent-session-codemode.test.ts > adds the usage of nested results to the codemode result`, with
`expected undefined to match object { input: 200, totalTokens: 200, ... }`. `gh run rerun --failed`
on the same commit passed, so it is an intermittent Bun-runtime difference, not a regression from this
change. It is recorded as a follow-up, not fixed here.

### Design deviations from this note, and why

- **`policy` did not lose its install, it lost its job.** `scripts/validate-workflows.mjs` imports the
  `yaml` package, so `policy` cannot run without `node_modules`. Instead the validation moved to
  `static` (which has them, and already runs the same script through `npm run check`) and `policy` now
  runs only `scripts/ci-impact.mjs`, which imports Node built-ins alone. That removes the install and
  the `docker run rhysd/actionlint` step from the critical path's first job: `policy` is 12 s.
- **`static` installs its own dependencies** rather than consuming the workspace artifact. At this
  stage there is no workspace artifact yet, and an uncached install is still faster than serializing
  behind the build. It keeps reporting even when `prepare` fails, which matters for diagnosis.

### Bun-only failure: codemode worker slots (root-caused and fixed)

The intermittent `Bun tests (coding-agent)` failures on hosted runners were not random. The message
was `Script sandbox failed: Codemode worker limit reached (2 workers are still running or shutting
down)`, and the cause is a runtime difference in worker shutdown:

- The sandbox holds a worker slot until the worker emits `exit`, not when `terminate()` is called. The
  limit is two. That rule is correct: releasing at `terminate()` would let a still-running worker be
  joined by a new one.
- Bun can take longer than Node to finish `terminate()`, and `protocol.ts` documents that Bun's
  `terminate()` cannot stop a thread spinning in wasm. A test that ends a script therefore can leave its
  worker in shutdown.
- `agent-session-codemode.test.ts` cleaned up harnesses synchronously and started the next test at once,
  so under Bun two earlier workers could still hold slots.

The fix is in the test, not the limit: the suite now waits, with a bounded timeout, until a read-only
`codemodeWorkersInFlight()` reaches zero. A full local Bun run of coding-agent went from seven failures
to two, and the two codemode failures are gone. Run `39045207907` is green with the change.

The remaining local failures (`session-id-readonly`, `startup-session-name`, `stdout-cleanliness`) are
read-only and CLI-output tests. They fail on this Windows machine and were not verified on Linux; the CI
Bun job is the check for them.

### Measured pull-request pipeline, with the critical path

| run | change | wall clock | result |
| --- | --- | --- | --- |
| `39038704551` | self-hosted `prepare`, `supply-chain`, `deep-gate` | 10.0 min | green, but `prepare` queued 260 s behind two other pull requests |
| `39039960657` | everything hosted | 4.8 min | one Bun flake, green on rerun |
| `39043358794` | apt reduced to fd and ripgrep, durable forwarding | 5.0 min | Bun codemode failure (root-caused above) |
| `39045207907` | codemode wait | 4.3 min | green, 33 of 33 |
| `39047357144` | workspace restored, coding-agent and durable sharded | 4.4 min | 4 failures: `openai` missing from the archive |
| `39048565260` | nested `node_modules` packed | 4.4 min | green, 40 of 40, but the Bun matrix ran in four waves of width 4 |
| `39049211499` | matrix widths 12 (Node) and 8 (Bun) | **3.48 min** | green, 40 of 40, peak 20 live jobs |

Critical path in `39049211499`: `policy` 11 s, `prepare` starts at +14 s and runs 39 s (install, build, pack,
upload), the matrix starts at about +58 s, the longest shard ends about 200 s, and `Merge gate` runs 8 s.
Deep verification (fuzz plus mutation) is the other long job in the high-risk path; it ends inside the same
window.

### Found while implementing

- **npm workspaces nest a conflicting dependency.** `openai` is installed in `packages/ai/node_modules`, not at
  the root. The first archive packed only the root `node_modules`, and every restored job failed with
  `Cannot find package 'openai'`, including the Cloudflare compat test that uses that client. Pack every
  `packages/*/node_modules` too. Checked locally: the archive holds `packages/ai/node_modules/openai/package.json`.
- **npm swallows `--shard=`.** `npm run test:bun -- durable --shard=1/64` ran the full package. The sharded
  jobs therefore call `node` directly, and `scripts/test-bun.mjs` takes `--shard=i/n` itself.
- **A matrix width is a cap, not a wait.** With `max-parallel` at 4 for Bun, 14 jobs ran in four waves and
  the gate waited for the last wave. The account limit is 20 live jobs; a wider matrix is queued by GitHub,
  not failed, so the widths are set above what can run and the account limit does the scheduling.
- **The archive is stored uncompressed by upload-artifact.** `compression-level: 0`; the file is already
  gzip, so a second wrapper would only cost time. The archive is 101 MB.

### Phase 3 results: nightly, release, daily scan

| workflow | run | runner | wall clock | notes |
| --- | --- | --- | --- | --- |
| nightly | `39071156179` | self-hosted | 13.6 min | failed: `Node tests (durable 3/3)`, see below. Dispatched together with the release run below, which shared the host |
| nightly | `39080623279` | self-hosted | **9.1 min** | green, 34 jobs. `prepare` 38 s, `deep` 151 s (fuzz 10,000 runs 1 s, mutation 116 s). The last small packages queued up to 460 s behind the width-2 matrices |
| release dry run | `39071159011` | self-hosted | 15.4 min | green, shared the host with the nightly above |
| release dry run | `39081319777` | self-hosted | 9.9 min | green. `prepare` 38 s, `check` 46 s, test matrix ends at +402 s, `pack` 34 s, `binaries` 72 s, `publish` 34 s |
| release dry run | `39082114809` | all hosted (31 of 31) | **5.7 min** | green. `prepare` 28 s, `check` 54 s, test matrix ends at +166 s, `pack` 25 s, `binaries` 65 s, `publish` 29 s |
| npm-audit | `39082190075` | hosted | - | green |

There is no earlier nightly run to compare with: the only two nightly runs ever recorded on this repository are
the first two rows above, both of the new shape, so the previous "30+ minutes of serial work" in the Nightly
section is an estimate, not a measurement.

What was built differs from the targets above in these ways:

- **Nightly stays self-hosted** (decided with the owner). It is not on any pull-request path, so queueing on
  the shared host cannot delay a review. It is one `prepare` job plus a Node matrix, a Bun matrix and a
  fuzz-and-mutation job that run in parallel; `recovery` is gone because its package list repeated
  `node-full`. The widths are 2 + 2 + 1 live jobs; a wider matrix on six cores is what produced load 17-26
  and the timing flakes. It is not a `workflow_call` shared with `ci.yml`: the three workflows still carry
  their own copy of the shard steps. That duplication is real and is left for a follow-up.
- **Release moved to hosted entirely**, not only the native build. The `verify` job is split into
  `prepare` (build, pack the workspace, expand the shards), `check`, a shard matrix and `pack`, and
  `pack` runs only after the first three pass, with `--skip-build` over the restored workspace. `publish`
  still checks that the manifest's commit equals `GITHUB_SHA` and that it was not dirty.
- **The binaries job no longer rebuilds.** It used to run `npm run build:offline` before compiling, so the
  executables embedded a `dist/` that no test had run against. It now restores the same workspace archive
  as the test shards and the pack job. The native prebuilds are downloaded after the restore; the archive
  holds no `native/` directory, so nothing is overwritten.

### Failures found after the merge, and what was done

- **`experimental-remote-runtime` failed once on `main`** (run `39072482535`, Bun, coding-agent 2/2) and passed
  on rerun, with identical code. Both sessions of the second server were missing from the list, not one.
  `discoverUnixServers` gives each probe 1000 ms and silently omits a server whose probe times out; the
  timer measures wall time, so a process that did not run for that long drops a healthy server. Reproduced
  deterministically on Linux with Bun 1.4.2 by blocking the event loop while the probes are in flight (0 or
  1 of 2 servers found; Node finds 2 of 2, which matches the failure never appearing in a Node shard). The
  fix (PR #41) grants one more window when the timer fires more than 50 ms late. The unit test failed 3 of 3
  before the change and passes 5 of 5 after. **Not shown:** the CI failure itself was not reproduced under
  natural load; the mechanism and the symptom agree, but another trigger is not excluded. If the same
  assertion fails again after #41, investigate again.
- **`Node tests (durable 3/3)` failed in the first nightly** on `tools-images` ("prepares in a worker
  thread, leaving the event loop free": 153 ms against a 100 ms bound). The nightly and a release dry run
  had been dispatched together onto the same six runners. The second nightly, alone, passed. The assertion
  measures event-loop lag, so it is load-sensitive by construction; it was not changed. Do not dispatch
  nightly and release together, and keep release off the shared host (it is now).
- **One workspace download stalled** for more than 15 minutes in `download-artifact` (process idle, no log
  line) in release dry run `39079138864`; the other 59 took 13-16 s. Every restore is now limited to 3 minutes
  and retried once for 5 (PR #42). A probe confirmed that a step that times out with
  `continue-on-error` lets the next step see `outcome == 'failure'` and run. The retry branch itself has
  not been exercised by a real stall.

### OCR review

OCR now runs in daily and pre-release batches, using `gpt-6.1-sol` at the dispatch service's maximum `high` effort. It is advisory and not part of `Merge gate`; the implemented decision and verification are in [the OCR note](../../implemented/process/2026-10-18-ocr-review-cadence.md). Repository Issues are currently disabled, so each result is kept in the run summary and a 30-day artifact.

### Still unverified, and each needs a run before the corresponding claim is made

1. ~~**Real concurrent-job limit**~~: measured at 20 (probe `39038007064`).
2. **Ubuntu-latest without the cairo/pango libraries**: verified in run `39043358794` and `39045207907`: `build:offline`, `npm run check`, the coding-agent and durable suites, and `test:mcp-conformance` pass with those five `apt-get` packages removed.
   **fd and ripgrep are not removable.** The probe step printed `image before: rg=missing fd=missing fdfind=missing` on `ubuntu-latest`, and with `NYAN_OFFLINE` set `ensureTool` returns undefined rather than downloading, so the `find` and `grep` tools would be unavailable. They are installed conditionally, only when missing (about 12 s when needed).
3. ~~**Workspace artifact**~~: verified in `39049211499`. The prepare job packs node_modules, every nested
   `packages/*/node_modules`, the built packages and the model-data lock into a 101 MB gzip; every test job
   restores it and runs without `npm ci`. The restored `@ny4n/*` workspace links resolve (the tests import
   each other through them).
4. ~~**`bun --bun <vitest> --run --shard=i/n`**~~: verified. `scripts/test-bun.mjs` forwards `--shard=i/n`; the
   sharded Bun jobs in `39049211499` pass, and locally `--shard=1/64` on durable ran two files and 22 tests.
5. **Durable shard forwarding**: verified. `scripts/test-without-document-retention.mjs` now forwards `process.argv.slice(2)`; `--shard=1/64` ran two files and 22 tests.
6. ~~**`static` without `dist`**~~: verified in run `39039960657` — `static` starts 3 s after `policy`,
   concurrently with `prepare`, and passes. Source-level support: `check-architecture-layers.mjs` skips
   `dist`, `check-entry-graphs.mjs` maps `./dist/x.js` onto `src/x.ts`, `check-ts-imports.mjs` ignores
   `dist`, and tsconfig maps every `@ny4n/*` specifier to sources.
7. ~~**Release timing**~~: measured, 5.7 min all hosted (run `39082114809`). A real tag has not been pushed; only dry runs.
8. ~~**Nightly**~~: measured, 9.1 min (run `39080623279`). The scheduled trigger has not fired yet on the new shape. Scheduled runs on this repository start hours after their cron time: the daily scan (cron 07:37 UTC) started 4.1, 5.1, 5.2, 5.4, 7.6 and 9.3 hours late over the last six days (median 5.4 h). At 02:40 UTC, 23 minutes after its cron time, the nightly (cron 02:17 UTC) had not started. So the cron time is not the start time: if the nightly has to finish before a given hour, the cron has to be set earlier by the observed delay, and that delay is not controlled by this repository.

Owner actions that are not workflow edits:

- Rotate the GitHub PAT found in plaintext in `/root/.npmrc` on `nyan-ci` (`//npm.pkg.github.com:_authToken=ghp_...`). Every self-hosted runner on that host can read it, and it will outlive this migration.
- Confirm the plan's concurrent-job limit in Settings -> Billing -> Actions, and record it in `policy.json`.
- ~~Decide whether branch protection requires `Merge gate` alone or plus `Policy and impact`~~: `Merge gate` alone is set on `main` (strict, applies to admins, no force push, no deletion).
- Decide whether to remove the six `nyan` self-hosted runner services. Nothing on a pull-request path uses them now; nightly and OCR still do. The original proposal was a one-week window first.
- ~~Decide the OCR cadence~~: implemented as daily and pre-release batches; see [the OCR note](../../implemented/process/2026-10-18-ocr-review-cadence.md).
