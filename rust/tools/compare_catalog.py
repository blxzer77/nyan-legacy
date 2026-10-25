#!/usr/bin/env python3
"""Compares the Rust catalog with the TypeScript model data.

    npm run hydrate:model-data
    cargo run -q -p nyan-coding-agent-models --example dump -- ~/.nyan/agent > /tmp/rust_catalog.json
    python3 rust/tools/compare_catalog.py /tmp/rust_catalog.json

Prints, per provider, how many models only one side has and how many shared
models differ in API, base URL, context window, output limit, or price. The two
data sets are fetched at different times, so some id differences are drift.
"""
import sys
import json,glob,os
rust=json.load(open(sys.argv[1]))
base=os.path.join(os.popen('git rev-parse --show-toplevel').read().strip(),'packages/ai/src/providers/data/')
print(f"{'provider':24} {'ts':>4} {'rs':>4} {'only-ts':>7} {'only-rs':>7} {'api≠':>5} {'base≠':>5} {'ctx≠':>5} {'max≠':>5} {'cost≠':>5}")
tot=[0]*7
for f in sorted(glob.glob(base+'*.json')):
    pid=os.path.basename(f)[:-5]
    d=json.load(open(f)); ts={}
    for api,models in d.items():
        for k,m in models.items():
            if m.get('api') in ('openai-decisions','typesafe-system-one','openrouter-images','cloudflare-workers-ai-system-one','llama-cpp-classify'): continue
            ts[m['id']]=m
    rs=rust.get(pid)
    if rs is None: continue
    same=set(ts)&set(rs)
    row=[len(set(ts)-set(rs)),len(set(rs)-set(ts)),
      sum(1 for i in same if ts[i]['api']!=rs[i][0]),
      sum(1 for i in same if (ts[i].get('baseUrl') or '')!=rs[i][1] and pid!='azure'),
      sum(1 for i in same if ts[i].get('contextWindow')!=rs[i][2]),
      sum(1 for i in same if ts[i].get('maxTokens')!=rs[i][3]),
      sum(1 for i in same if abs((ts[i].get('cost') or {}).get('input',0)-rs[i][6])>1e-9 or abs((ts[i].get('cost') or {}).get('output',0)-rs[i][7])>1e-9)]
    for k,v in enumerate(row): tot[k]+=v
    if any(row): print(f"{pid:24} {len(ts):>4} {len(rs):>4} "+" ".join(f"{v:>7}" if k<2 else f"{v:>5}" for k,v in enumerate(row)))
print("TOTAL only-ts/only-rs/api/base/ctx/max/cost:",tot)
