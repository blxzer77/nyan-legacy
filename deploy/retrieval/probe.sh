#!/usr/bin/env bash
# Readiness probes for the self-hosted retrieval services. Each check prints
# PASS/FAIL with the observed value; exit code is the number of failures.
#
# These are the cheap checks the contract needs before a provider may serve
# candidates (an HTTP 200 is not readiness). They are not the full
# pactile.cg.readiness-fixture set.

set -uo pipefail

EMBED_URL="${EMBED_URL:-http://127.0.0.1:8090}"
RERANK_URL="${RERANK_URL:-http://127.0.0.1:8091}"
QDRANT_URL="${QDRANT_URL:-http://10.144.0.3:6333}"
QDRANT_KEY_FILE="${QDRANT_KEY_FILE:-$HOME/retrieval/keys/qdrant-api-key}"
EMBED_API="${EMBED_API:-llama}"
EXPECTED_DIM="${EXPECTED_DIM:-1536}"
export EXPECTED_DIM

fails=0
pass() { printf 'PASS  %s\n' "$*"; }
fail() { printf 'FAIL  %s\n' "$*"; fails=$((fails + 1)); }

jq_py() { python3 -c "$1"; }

probe_embed() {
  python3 "$(dirname "$0")/probe-embed.py" || fails=$((fails + 1))
}

probe_rerank() {
  echo "== rerank: $RERANK_URL"
  local out
  # `-f` so a non-2xx answer is this function's failure instead of JSON that happens to parse.
  if ! out="$(curl -sf --max-time 60 -X POST "$RERANK_URL/score" -H 'content-type: application/json' -d '{
    "model": "qwen3-reranker-0.6b",
    "text_1": "how do durable tasks checkpoint a phase",
    "text_2": [
      "The harness commits an intent row, performs the effect, then commits the outcome; on restart it replays only intents whose outcome is missing.",
      "The weather in Paris is mild in spring and people enjoy walking along the Seine."
    ]
  }')"; then
    fail "rerank request failed"
    return
  fi
  # The checks print one `CHECK <name> PASS|FAIL` line each; the count comes from those lines, because a printed FAIL is
  # the verdict and the helper's own exit code cannot carry how many of them there were.
  local checks
  checks="$(printf '%s' "$out" | jq_py '
import json,sys
try:
    d=json.load(sys.stdin)
except Exception as error:
    print("CHECK answer_parses    FAIL %s" % error); sys.exit(0)
data=d.get("data") or d.get("results")
if not data:
    print("CHECK score_shape      FAIL %r" % (d,)); sys.exit(0)
scores=[x.get("score") for x in data]
print("CHECK score_shape      PASS n=%d" % len(scores))
ok=all(isinstance(s,(int,float)) and not isinstance(s,bool) and 0.0<=s<=1.0 for s in scores)
print("CHECK score_range_0_1  %s %r" % ("PASS" if ok else "FAIL", scores))
rel=len(scores)==2 and scores[0]>scores[1]
print("CHECK ranking_sanity   %s" % ("PASS" if rel else "FAIL"))
') "
  if [[ -z "$checks" ]]; then
    fail "rerank produced no checks"
    return
  fi
  printf '%s\n' "$checks"
  local failed=0
  failed="$(printf '%s\n' "$checks" | grep -c ' FAIL' || true)"
  fails=$((fails + failed))
}

probe_qdrant() {
  echo "== qdrant: $QDRANT_URL"
  if [[ ! -r "$QDRANT_KEY_FILE" ]]; then fail "no api key file at $QDRANT_KEY_FILE"; return; fi
  local key; key="$(tr -d '\n' <"$QDRANT_KEY_FILE")"
  local code
  code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$QDRANT_URL/collections")"
  [[ "$code" == "401" ]] && pass "anonymous request refused (401)" || fail "anonymous request answered $code"
  # The key goes through a 0600 config file rather than `-H`: an argument is readable by anyone who can run `ps`.
  local cfg; cfg="$(mktemp)"
  chmod 600 "$cfg"
  printf 'header = "api-key: %s"\n' "$key" >"$cfg"
  code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 --config "$cfg" "$QDRANT_URL/collections")"
  rm -f "$cfg"
  [[ "$code" == "200" ]] && pass "authenticated request ok (200)" || fail "authenticated request answered $code"
}

case "${1:-all}" in
  embed) probe_embed ;;
  rerank) probe_rerank ;;
  qdrant) probe_qdrant ;;
  all) probe_embed; probe_rerank; probe_qdrant ;;
  *) echo "usage: $0 [embed|rerank|qdrant|all]" >&2; exit 2 ;;
esac

echo
echo "failures: $fails"
exit "$fails"
