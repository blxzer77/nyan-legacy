# Nyan retrieval infrastructure (ops record)

> **Where this lives.** This directory is the source: the compose file, the probe and bench scripts, and this record.
> Runtime state stays outside it and outside the repository — `keys/` (the Qdrant API key), `vllm-cache/` (the weight
> cache, ~42 MB) and `last-probe.txt`, all covered by the local `.gitignore`.
>
> The deployed copy on `nyan-dev-02` is `/opt/stacks/retrieval`, and the workstation units are
> `~/.config/containers/systemd/retrieval-*.container` and `~/.config/systemd/user/retrieval-probes.service`. Editing a
> script here does nothing until it is copied there; the two paths are recorded so that is not a surprise.

Standing services for the retrieval roadmap (N6/N8), deployed before any
`@ny4n/retrieval` code exists. Ops material, not product code.

Scope decisions these services implement: self-use/research (so `cc-by-nc-4.0`
weights are acceptable), reranker = smallest model that does not bottleneck the
retrieval budget (0.6B; revisit only if quality proves insufficient).

## Topology

| Piece | Host | Endpoint | Runtime |
| --- | --- | --- | --- |
| Embedding: `jinaai/jina-code-embeddings-1.5b` (BF16 GGUF) via llama.cpp | this workstation, RTX 3060 Laptop 12 GB | `http://127.0.0.1:8090` | rootless podman, user unit `retrieval-llama-embed` |
| Rerank: `Qwen/Qwen3-Reranker-0.6B` via vLLM `--runner pooling` | this workstation | `http://127.0.0.1:8091` | rootless podman, user unit `retrieval-vllm-rerank` |
| Vector backend: Qdrant 1.19.2 | nyan-dev-02 (6 vCPU, 23 GB, no GPU) | `http://10.144.0.3:6333` (+ loopback) | docker compose, `/opt/stacks/retrieval` |
| Coordinator metadata DB: PostgreSQL 17.11 (`retrieval_coord`, own role) | nyan-dev-02, inside the langfuse Postgres container | `127.0.0.1:5433/retrieval_coord` | row in `/opt/stacks/retrieval/keys/coord-db.env` |
| Observability/eval plane: Langfuse 4 API | nyan-dev-02 | `http://10.144.0.3:50007/langfuse-api/` | existing langfuse stack, new nginx location |

GPU services are Quadlet units: `~/.config/containers/systemd/retrieval-*.container`.
Operate with `systemctl --user {start,stop,status} retrieval-llama-embed retrieval-vllm-rerank`;
logs go to `journalctl --user -u <unit>`. Linger is enabled, so they start at boot
without a docker daemon. `compose.gpu.yaml` is the docker equivalent, kept for
reference only — nothing runs from it.

Both GPU services bind loopback only: the client runs on this host, so there is
no mesh exposure and no service auth. Qdrant binds `10.144.0.3` and
loopback and requires its API key (anonymous requests get 401).

## Verified (2026-10-20)

`./probe.sh all` — exit code is the failure count. The qdrant line below is the current mesh address; that run
printed the retired Tailscale address `100.80.19.12`.

```
== embedding (llama): http://127.0.0.1:8090
PASS  GET /props n_ctx=8192
PASS  dim 1536 (expected 1536)
PASS  finite
PASS  unit_norm 1.000000
PASS  batch_32 n=32
PASS  batch_aligned
PASS  distinct_texts cos=0.4446
== rerank: http://127.0.0.1:8091
CHECK score_shape      PASS n=2
CHECK score_range_0_1  PASS [0.587, 0.211]
CHECK ranking_sanity   PASS
== qdrant: http://10.144.0.3:6333
PASS  anonymous request refused (401)
PASS  authenticated request ok (200)
```

Measured on this GPU (`bench-embed.py`, `bench-rerank.py`), 400-token code
chunks, single client:

| Measurement | Result |
| --- | --- |
| document embed throughput | 15.1 chunks/s (256 chunks in 17.0 s) -> 10k chunks ~11 min, 50k ~55 min |
| query embed latency | p50 18 ms, p90 23 ms |
| rerank 20 candidates | p50 350 ms |
| rerank 50 candidates | p50 546 ms |
| VRAM in use (both services + desktop) | 8.5 / 12.3 GB |

Rerank is not the bottleneck: 50 candidates costs ~0.5 s, so a 4B scorer
(~6x prefill, ~3.5-4 s for 50) would still fit a single-digit-second budget, but
it needs ~2.4 GB quantized weights to coexist with the 3.4 GB embedding model.
Deferred until an eval set shows the 0.6B is insufficient.

## Configuration that matters (goes into the retrieval generation tuple)

- Embedding: 1536 dims, last-token pooling, L2-normalized output, 8192-token
  context per slot, one slot (`--parallel 1`).
- Task prefixes are not applied by the service. The model repo's
  `config_sentence_transformers.json` defines them (`nl2code_query`,
  `nl2code_document`, `code2code_*`, `qa_*`, ...); the client prepends the chosen
  prefix as text so the prompt profile stays explicit and versioned.
- Matryoshka: 128 / 256 / 512 / 1024 / 1536. The service always emits 1536; a
  smaller profile is client-side truncation + renormalization, recorded in the
  tuple.
- Rerank: Qwen3-Reranker scores by the `no`/`yes` token pair, so vLLM needs the
  documented override
  `--hf-overrides '{"architectures":["Qwen3ForSequenceClassification"],"classifier_from_token":["no","yes"],"is_original_qwen3_reranker":true}'`.
  `/score` returns probabilities in [0, 1].

## Notes that cost time (keep them)

- **Rootless podman is the runtime here.** The workstation's active docker context
  is `podman-rootless`, so `docker` was already podman; the GPU setup lives in
  podman. Two non-default pieces are required:
  - user CDI spec: `nvidia-ctk cdi generate --output=$HOME/.config/cdi/nvidia.yaml`
    plus `cdi_spec_dirs` in `~/.config/containers/containers.conf`, then
    `--device nvidia.com/gpu=all`;
  - `--security-opt label=disable` (`SecurityLabelDisable=true`): raw CDI device
    nodes are `device_t` and `container_t` cannot use them, so without it NVML
    fails with "Insufficient Permissions" although the nodes are mode 0666.
- **Images.** Podman cannot reuse docker storage; images were moved with
  `skopeo copy docker-daemon:<ref> containers-storage:<ref>` (3 min for 7 GB,
  ~20 min for the 33 GB vLLM image) rather than a second network pull. The vLLM
  image is big because it is the whole torch stack.
- **TEI cannot serve this model here.** TEI `cuda-1.9` loads
  `jina-code-embeddings-1.5b`, allocates 3.2 GB, then spins at 100% CPU without
  binding its port (FlashQwen2 sm80 kernels on an sm86 GPU). `--dtype float32`
  fails with "Could not start a suitable backend". Also, the Jina repo ships no
  `tokenizer.json`; TEI needs the one from `Qwen/Qwen2.5-Coder-1.5B` (vocab and
  merges verified identical, jina's `merges.txt` only adds the `#version` header).
  llama.cpp serves the same weights from Jina's BF16 GGUF, which is what runs.
- **CLI forms.** llama.cpp rejects `--model=/path` (use `-m /path`); compose
  rejects unquoted numeric args (quote them). The vLLM image entrypoint is already
  `vllm serve` (do not prepend `serve`), and `--task score` no longer exists.
- **vLLM cold start** is ~3 min (torch.compile). The unit mounts
  `~/retrieval/vllm-cache` at `/root/.cache` so restarts reuse the compile cache.

## Prepared ahead of the code

- **Coordinator DB** on dev-02: database `retrieval_coord` with its own login role,
  created in the existing PostgreSQL 17 container (langfuse's), reachable only on
  `127.0.0.1:5433`. The role owns an empty database and sees no langfuse tables.
  Credentials: `/opt/stacks/retrieval/keys/coord-db.env` (0600).
- **Langfuse API** at `http://10.144.0.3:50007/langfuse-api/` (nginx location
  proxying `/api/public/`). The browser UI is `http://10.144.0.3:8448`. Verified
  with `GET /langfuse-api/health` (200) and an authenticated
  `GET /langfuse-api/projects` returning project `nyan`. Keys for the client are in
  `/opt/stacks/retrieval/keys/langfuse.env` (0600, `pk-lf-nyan-local` /
  `sk-lf-nyan-local`, the project langfuse was initialised with).
- **End-to-end smoke test** (`smoke-e2e.py`): embeds 6 code chunks, creates a Qdrant
  collection with the contract's payload indexes (`worktreeId`, `scopeId`,
  `generation`, `path`), upserts with source-identity payloads, runs a
  task-prefixed query with a generation filter, reranks the top 5, takes a
  collection snapshot, and deletes it. All steps pass — the snapshot means the
  staged-generation/rollback path is available, not just assumed.
- **Hourly probes**: `systemctl --user` timer `retrieval-probes.timer` runs
  `probe.sh all` and writes `last-probe.txt`.

## Not deployed yet (needs code, not config)

- index coordinator service (active-manifest CAS, epoch/fencing, job leases) — its
  database exists, the service does not
- strict Tantivy/BM25 service
- external search provider (N8) and the SSRF-safe fetch path
- eval corpus + gold query set (decides whether the 0.6B reranker is enough)
- backup plan: backrest runs on dev-02 with `/opt/stacks` mounted at
  `/userdata/stacks` but has no repos or plans configured, and retention is still
  an owner decision (roadmap G8). Qdrant's own snapshot API works; no snapshot
  job or prune policy is installed.

## Files

- `~/.config/containers/systemd/retrieval-*.container` — the two GPU units
- `~/.config/containers/containers.conf`, `~/.config/cdi/nvidia.yaml` — rootless GPU plumbing
- `probe.sh`, `probe-embed.py` — readiness probes (exit code = failures)
- `bench-embed.py`, `bench-rerank.py` — throughput/latency measurement
- `keys/qdrant-api-key` — Qdrant credential copied from dev-02
- `compose.gpu.yaml` — docker reference, not used
