# Agent Note: Nyan quality workflow architecture

Status: proposed: target design implemented in repository files; GitHub-side Runner labels, branch protection, and activation remain pending

## Problem

Nyan is a 12-package TypeScript workspace containing an LLM provider layer, an agent loop, a durable runtime, a CLI/RPC product shell, a TUI, protocol packages, and Bun/native release artifacts. The repository already has broad CI coverage, but its current workflow combines large checks, repeats setup work, generates model data from the network during builds, and does not yet use risk-based deep verification. The workflow must maximize both quality and feedback speed.

## Decision

The target workflow is a single final architecture with staged execution, not multiple workflow versions:

- Scope is code through public release: pull requests, main validation, Nightly deep verification, security, AI review, native artifacts, npm publication, and GitHub Release. Production deployment of Nyan users is outside this workflow.
- PR feedback targets p95 under 10 minutes. Changed-file impact analysis selects tests; if impact analysis is uncertain, the workflow fails closed and escalates to the full suite.
- High-risk changes enter a blocking deep gate with a maximum wait of 30 minutes. Deep checks include durable recovery, incremental mutation testing, property/Fuzz budgets, provider/protocol failure cases, real CLI/RPC smoke, and validated AI findings.
- AI findings are advisory unless a generated minimal reproduction succeeds in an isolated, credential-free validation job (or a deterministic rule independently confirms the finding). AI Review runs on the OCR/AI service on `nyan-ops` and does not become a generic unverified blocking check.
- Ordinary CI uses a fixed, hash-verified model-data snapshot and `npm run build:offline`. Network model generation runs only in a separate scheduled update workflow that creates an update PR.
- `nyan-ci` is the dedicated Nyan CI host. `nyan-ops` remains dedicated to OCR/AI and its existing services; it is not an ordinary CI Runner pool member.
- Runner roles are separated by labels and permissions: fast/static, test, heavy/integration, and protected release. The 6-vCPU/24-GiB `nyan-ci` host uses warm runners and bounded concurrency rather than running every heavy job concurrently. System dependencies are preinstalled or baked into the runner image; workflows do not run repeated `apt-get` setup.
- External Fork PRs run only safe checks on GitHub-hosted runners. They do not receive self-hosted runner access, internal Docker access, secrets, or private AI endpoints.
- Node and Bun tests run in package/impact matrices. The durable package keeps both normal document-retention and zero-retention test modes. Main and Nightly perform full validation.
- Release publishes exactly the artifacts produced and verified by the release verification job. The publish job does not rebuild packages independently. Checksums, native artifacts, standalone binaries, smoke tests, provenance, npm publication, and GitHub Release remain part of the release path.
- The initial quality stack is repository-native checks plus focused open tooling: Gitleaks, actionlint, Semgrep rules, Trivy, SBOM generation, dependency/license checks, Node/Bun tests, property testing, mutation testing, protocol conformance, durable crash/recovery tests, and OCR. SonarQube, Keploy, Testcontainers, Chaos Mesh, and full DAST are not required for this repository's current risk profile.

The intended workflow responsibilities are:

```text
pr.yml       impact, policy, prepare, static, selected Node/Bun tests,
             Windows smoke, MCP conformance, supply chain, fast AI review,
             30-minute deep gate for high-risk changes, final merge gate
main.yml     full Node/Bun/platform validation on main
nightly.yml  mutation, fuzz, crash/recovery, full platform tests, performance,
             dependency/license/security and deep AI review
security.yml action, secret, dependency, SBOM and license checks
ocr-review   internal AI review and evidence-validation handoff
native.yml   platform native module builds and artifact checks
release.yml  verify once, package once, smoke, attest, publish exact artifacts
```

## Alternatives considered

- One monolithic `build + check + test` PR job: rejected because it wastes parallelism and makes failures and timing opaque.
- Full repository tests on every PR: rejected because the repository is large and the same quality can be obtained with fail-closed impact analysis plus full main/Nightly validation.
- All jobs on one unrestricted self-hosted Runner label: rejected because Docker/release permissions and CPU contention would be mixed.
- All jobs on `nyan-ops`: rejected because it hosts OCR, Langfuse, ClickHouse, databases, and other long-running services whose load would make CI timing unstable.
- Fully ephemeral cold-start Runners: rejected as the default because setup latency would conflict with the 10-minute target; warm CI Runners with bounded concurrency are preferred.
- Live model catalog generation in every build: rejected because the same commit would not be reproducible.
- Direct AI blocking based on confidence or natural-language findings: rejected because unvalidated hallucinations must not block merges.
- SonarQube/Keploy/Testcontainers/Chaos Mesh as mandatory initial layers: rejected because they do not target Nyan's current primary risks as efficiently as durable recovery, provider-stream, protocol, CLI/RPC, and runtime-parity tests.

## Consequences

The design requires:

- a machine-readable `scripts/ci-impact.mjs` and a fail-closed gate aggregator;
- fixed model-data snapshot storage with immutable hashes;
- stable GitHub required-check names and merge-queue support;
- Runner label and concurrency reconfiguration on `nyan-ci`;
- a safe validation sandbox for AI-generated reproductions;
- release artifact reuse instead of a second independent build;
- timing, JUnit, SARIF, coverage, mutation, Fuzz-seed, model-snapshot, and build-manifest artifacts.

The design deliberately accepts that a high-risk PR can wait up to 30 minutes, while keeping ordinary PRs within the 10-minute target. It also keeps GitHub and npm as external collaboration/publication boundaries; the quality, Runner, AI, snapshot, and scanning components remain self-hosted or locally controlled.

## Verification

- Inspected the latest repository tree and `AGENTS.md`, `ROADMAP.md`, existing workflows, package scripts, test configuration, architecture rules, and model-data generation/validation code.
- Confirmed `nyan-ci` through the relay path: 6 vCPU, 24 GiB RAM, about 256 GiB free disk, Docker 29.9, low load, multiple Nyan Runner services, registry/mirror and BuildKit.
- Confirmed `nyan-ops`: 6 vCPU, 24 GiB RAM, about 212 GiB free disk, Docker 29.9, OCR and observability/provider services, and materially higher service load. No remote configuration was changed.
- Repository implementation now includes the impact classifier, fixed model-data snapshot restore/verification, workflow policy validation, package artifact packing/publishing, PR/main/nightly/security workflows, action pinning, and the final release artifact handoff.
- The deep gate currently runs the repository's existing critical recovery, protocol, Agent, AI, durable, and product suites. Stryker/Fuzz harnesses are not present in the source tree, so no fake mutation/Fuzz command was added; adding those harnesses is an explicit follow-up rather than a silently skipped quality claim.
- GitHub-side required checks, Runner label assignment, and live workflow execution remain unverified and must be activated after review.
