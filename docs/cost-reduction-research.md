# Research: cutting agent inference cost without dumbing the model down

**Status: research.** Nothing here has landed. These findings feed
[`agent-mode.md`](agent-mode.md) §7 (prompts), §9 (local models) and §10
(traces/evals), and they extend [`usage-accounting.md`](usage-accounting.md).

**Research baseline:** checked 2026-10-15 against repository commit
`fe77282983515b8b1c4e73c6934cf09167db53b3` (plus the uncommitted working tree)
and the primary sources listed at the end.

**Ground rule:** the user does not want a cheaper but worse model. Every
technique below lists the **measured quality impact** next to the savings.
Where a source measured no quality at all, the table says so.

## Recommendation

The cheapest token is one that is served from cache or never sent. Spend the
effort on **exact** savings (caching, prefix stability, deduplication) first.
They cannot change answers. Lossy context reduction (masking old tool output,
compaction) comes next, gated behind `nexus eval`. Prompt compression,
KV-cache eviction and per-step model routing are not worth it for nexus.

| # | Do now (exact or near-exact, low risk) | Where |
|---|---|---|
| 1 | **Let the Anthropic cache breakpoint move with the tool loop.** Today the second breakpoint is fixed when `build_history` runs, so every tool round's new output is billed at full price on every later round of that turn. | `app/chat.rs` `mark_cache_breakpoints`, `provider/openrouter.rs` `run_chat_loop` |
| 2 | **Send OpenRouter a `session_id`** (the existing `prompt_cache_key`) so sticky provider routing starts on the first request rather than after the first hit. | `provider/openrouter.rs` `add_prompt_cache_key` |
| 3 | **Static-first prompt assembly**, with `{{datetime}}` moved to the tail (already planned in agent-mode §7). | `app/chat.rs` `system_prompt`, `assets/system-prompt-base.md` |
| 4 | **Cover research and swarm sub-agent loops with breakpoints** (the known gap in usage-accounting.md). | `app/research.rs`, `app/swarm.rs` |
| 5 | **Local: keep the prefix stable and size the context.** Prompt-cache reuse is on by default in llama.cpp and mlx-lm. Ollama may start nexus with a 4k window, which is smaller than the tools plus the system prompt. | `provider/serve.rs` `LocalRuntime::launch` |

The "experiment first" and "not worth it" lists are in
[§3 Mapping](#3-mapping-to-nexus-chat).

## 1. What the evidence says

### 1.1 Prompt / prefix caching (exact: same tokens, same model)

Caching reuses computed attention state for an identical prefix. Outputs do
not change. vLLM's design doc calls it "almost a free lunch and won't change
model outputs" [S5]. llama.cpp warns that `cache_prompt` "can cause
nondeterministic results" because logits are not bit-identical across batch
sizes [S8]. That is numeric noise, not a quality loss.

| Provider / runtime | How | Price of write / read vs base input | TTL | Source |
|---|---|---|---|---|
| Anthropic | Explicit `cache_control` breakpoints (max 4), **or top-level `cache_control` = automatic caching**, which moves the breakpoint to the last cacheable block as the conversation grows | write 1.25× (5 min) or 2× (1 h); read 0.1× (0.05× on Opus/Sonnet 5.5, 0.025× on Fable/Mythos 5.1) | 5 min default, refreshed free on each hit; 1 h optional | [S1] |
| Anthropic, details | Writes happen **only at breakpoints**. Lookback checks ≤20 positions per breakpoint. Prefix order is tools → system → messages. Minimum prefix is 512–4,096 tokens depending on model | — | TTL counts from request **start** | [S1] |
| OpenAI ≤ GPT-5.5 | Automatic. `prompt_cache_key` helps routing (≈15 req/min per key) | write free; read model-dependent | `in_memory` 5–10 min (≤1 h) or `24h` (default for non-ZDR orgs) | [S2] |
| OpenAI GPT-5.6+ | Implicit breakpoint at the end of the latest message, plus optional explicit `prompt_cache_breakpoint` | write 1.25×; read 0.1× | `30m` | [S2] |
| OpenRouter | Passes `cache_control` through (explicit and top-level automatic for Anthropic). Sticky provider routing for 10 min, keyed by `session_id` / `x-session-id`, else `prompt_cache_key`, else a hash of the first messages | per upstream (table in [S3]); DeepSeek read 0.1×, Gemini implicit 0.25×, Grok/Moonshot 0.25× | per upstream | [S3] |
| vLLM | Automatic prefix caching: block-hash, LRU eviction. Saves **prefill only, not decode** | n/a (local) | memory-bound | [S5][S6] |
| SGLang | RadixAttention prefix tree; "up to 6.4× higher throughput" on agent/multi-turn/JSON workloads (whole system, not caching alone) | n/a | memory-bound | [S7] |
| llama.cpp server | `cache_prompt` on by default; `--cache-reuse N` (KV shifting); `--cache-ram` 8 GiB default; slot save/restore to disk via `--slot-save-path` | n/a | RAM-bound | [S8] |
| mlx-lm | `mlx_lm.cache_prompt` / `--prompt-cache-file`; `mlx_lm.server` keeps an LRU of prompt caches (`--prompt-cache-size` default 10, `--prompt-cache-bytes`) and reuses the nearest prefix | n/a | LRU | [S9][S10] |

**Agent-loop evidence.** *Don't Break the Cache* (arXiv 2601.06007, 2026) [S4]
ran 40 DeepResearch Bench sessions per condition with a 10k-token system
prompt:

| Model | Full-context cache: cost / TTFT | System-prompt-only cache: cost / TTFT |
|---|---|---|
| GPT-5.2 | −79.3% / −9.5% | −81.4% / −10.5% |
| Claude Sonnet 4.5 | −77.8% / −21.8% | −78.5% / −22.9% |
| Gemini 2.5 Pro | −38.3% / −6.0% | −41.4% / −6.1% |
| GPT-4o | −47.8% / **+8.8% (worse)** | −45.9% / −30.9% |

- **Quality was not measured** (cost and TTFT only). Caching is exact, so none
  is expected.
- The authors give a *hypothesis*, not a tested result, for why full-context
  caching can lose: writing dynamic tool results to cache costs more than it
  saves.
- Their recommendations: no timestamps, session IDs or user data at the top
  of the system prompt; a fixed tool set; and the warning that summarizing or
  pruning old tool calls breaks the cache.

**What breaks a cache hit in an agent loop:**

| Breaker | Effect | Source |
|---|---|---|
| Any byte change early in the prefix (timestamp, session id, reordered sections) | Everything after it misses | [S1][S2][S4] |
| Changing tool definitions | Anthropic: invalidates tools, system and messages. OpenAI advises `tool_choice`/`allowed_tools` instead of removing tools | [S1][S2] |
| Changing `tool_choice` | Anthropic: messages cache invalidated | [S1] |
| Adding or removing images in the prefix | Anthropic: messages cache invalidated | [S1] |
| Changing thinking/effort parameters; dropping earlier thinking blocks | Anthropic: messages cache invalidated | [S1] |
| Summarization, compaction, truncation, clearing tool results | Prefix reset. Anthropic's `clear_at_least` exists so that each clear removes enough tokens to justify the rewrite | [S2][S47] |
| Model switch (routers, cascades) | Caches are per model, so the new model starts cold | inferred from [S1][S2] |
| Idle > TTL (user think time) | Rewrite at 1.25× on Anthropic 5-min TTL | [S1] |

### 1.2 Context management for long agent sessions (lossy: needs evals)

| Technique | Source | Savings | Measured quality impact | Caveats |
|---|---|---|---|---|
| **Observation masking**: keep the last M=10 turns' tool output, replace older output with a placeholder, keep reasoning and actions | *The Complexity Trap* (JetBrains), arXiv 2508.21433, 2025 [S11] | ≈50% cost vs raw agent in 4/5 configs (e.g. Qwen3-Coder 480B $1.29→$0.61) | SWE-bench Verified solve rate: Qwen3-Coder 53.4→54.8, Gemini 2.5 Flash 32.8→35.6, Qwen3-32B-thinking 23.0→24.6; **but Qwen3-32B 17.0→15.0 and Gemini Flash-thinking 40.4→36.4** | The window had to be re-tuned for OpenHands ("degraded drastically" with SWE-agent's value). Mixed results on small/thinking models |
| LLM summarization (summarize 21 turns, keep last 10 verbatim) | same [S11] | 41–55% cost | Comparable, **but Gemini Flash-thinking 40.4→31.4 (−22%)**. Runs got ≈13–15% longer. Summary calls cost 0.65–7.2% of the total and get almost no cache reuse | Not better than masking. The hybrid (mask first, summarize late) was 7% cheaper than masking, with +2.6 pp on Qwen3-Coder (50-instance slice) |
| **Trajectory reduction** (AgentDiet): a cheap reflection model rewrites the step from 2 steps back, removing useless, redundant and expired content, and only when it saves >500 tokens | arXiv 2509.23586, FSE 2026 [S12] | input −39.9 to −59.7%; final cost −21.1 to −35.9% including the reducer | SWE-bench Verified: Claude 4 Sonnet 64.5→66.5, Gemini 2.5 Pro 50.5→52.0; Multi-SWE-bench Flash 40.0→39.0 / 21.7→22.7 (range −1 to +2 pp) | Rewrites are deliberately delayed and confined to one old step so earlier cache entries survive. Reducer overhead 5–12% |
| **ACON**: learned compression guidelines for history (>4,096 tokens) and observations (>1,024 tokens) | arXiv 2510.00615 (v3 2026) [S13] | peak tokens −26 to −54% | AppWorld (gpt-4.1): 56.0 → **56.5** with optimized guidelines, but **naive "compress this" prompting 43.5** and FIFO truncation 45.8. OfficeBench 76.84 → 72.63–74.74 | Naive summarization loses 12+ pp on an agent benchmark. Only optimized prompts held quality. Reported cost excludes caching; latency rose with a local compressor |
| Provider-side tool-result clearing (`clear_tool_uses_20250919`: trigger, `keep` = 3, `clear_at_least`) | Anthropic docs [S47] | as configured | **No quality numbers published** | Clearing invalidates the cache. Anthropic API only (not confirmed via OpenRouter) |
| MemGPT / Letta memory tiers | arXiv 2310.08560 [S14] | — | Abstract has no numbers | Architecture, not a cost result |
| Mem0 (retrieved memory instead of full history) | arXiv 2504.19413 [S16] | >90% tokens, p95 latency 17.1 s → 1.4 s | **LOCOMO J score 72.90 (full context) → 66.88 (Mem0)**: a 6-point loss vs full context | Conversational QA, not tool use. A trade, not free |
| A-MEM (Zettelkasten-style memory) | arXiv 2502.12110 [S15] | ≈1.2–2.5k vs ≈16.9k tokens per op | LoCoMo multi-hop F1 +0.4 to +2 (GPT-4o-mini), larger on small models; single runs, no error bars | Mem0's paper measured A-Mem at J 48.38 [S16]. The two papers disagree |
| ACE (incremental, itemized context updates) | arXiv 2510.04618, ICLR 2026 [S21] | "significantly" lower adaptation cost (no figure in abstract) | +10.6% on agent benchmarks, +8.6% finance (abstract) | Names "brevity bias" and "context collapse" as failure modes of iterative rewriting. Supports itemized ADD/UPDATE/DELETE memory over a rolling rewrite |

**Why shorter can be *better*, not just cheaper:**

| Finding | Source |
|---|---|
| U-shaped accuracy: information in the middle of a long context is used worst | Lost in the Middle, arXiv 2307.03172 (TACL) [S17] |
| At 32k tokens, 11 of 13 long-context models fall below 50% of their short-context score; GPT-4o 99.3→69.7 | NoLiMa, arXiv 2502.05167 [S19] |
| 18 models degrade as input grows, even on copy tasks; focused (~300-token) prompts beat full (~113k) on LongMemEval for every family; one distractor already hurts | Chroma *Context Rot*, 2025: a **vendor technical report, not peer-reviewed or on arXiv** [S18] |
| Multi-turn performance averages −39% vs single-turn across six tasks, mostly from unreliability | arXiv 2505.06120 [S20] |

### 1.3 Prompt compression (lossy, token-level)

| Method | Source | Reported savings | Reported quality | On agents? |
|---|---|---|---|---|
| LLMLingua | arXiv 2310.05736, EMNLP 2023 [S22] | up to 20× | "little performance loss" (GSM8K, BBH, ShareGPT) | — |
| LongLLMLingua | arXiv 2310.06839, ACL 2024 [S23] | ~4× fewer tokens; LooGLE −94% cost | NaturalQuestions **+21.4%** (relevant-doc reordering effect) | — |
| LLMLingua-2 | arXiv 2403.12968 [S24] | 2–5× compression, 1.6–2.9× e2e latency | "significant gains over baselines" (no figures in abstract) | — |
| Selective Context | arXiv 2310.06201 [S25] | −50% context, −32% time | BERTScore −0.023, faithfulness −0.038 | — |
| Comparison study | arXiv 2407.08892 [S26] | extractive up to 10× | Extractive reranking beats token pruning; abstractive gives marginal gains | — |
| **LLMLingua as an agent-history compressor** | ACON baseline [S13] | peak tokens 9.93k → 7.50k | **AppWorld 56.0 → 39.3 (−16.7 pp); OfficeBench 76.84 → 70.53** | **Yes. Clear quality loss on agentic tasks** |

### 1.4 KV-cache compression for local inference

| Method | Source | Savings | Quality | Runtime support today |
|---|---|---|---|---|
| **q8_0 KV cache** | llama.cpp PR #7412 (measurement PR) [S33]; Ollama FAQ [S34] | ≈½ KV memory | PPL 6.2322 → 6.2344 (f16 weights); "no significant quality loss" [S33]; "usually no noticeable impact" [S34] | llama.cpp `-ctk/-ctv q8_0` [S8]; Ollama `OLLAMA_KV_CACHE_TYPE=q8_0` [S34] |
| q4_0 KV cache | same | ≈¼ KV memory | PPL 6.2322 → 6.4384. **K is "much more sensitive" than V** [S33]; "more noticeable at higher context sizes"; high-GQA models such as Qwen2 lose more [S34] | same flags |
| mlx-lm `--kv-bits` | mlx-lm SERVER.md / server.py [S10] | as set (example 4) | **No owner-published quality numbers** | Quantized KV starts at token 5000 by default and **disables batching** |
| KIVI (2-bit asymmetric) | arXiv 2402.02750, ICML 2024 [S27] | 2.6× peak memory, 2.35–3.47× throughput | "almost the same quality" (qualitative in abstract) | Not in llama.cpp/mlx-lm flags |
| KVQuant (3-bit) | arXiv 2401.18079, NeurIPS 2024 [S28] | 1M ctx on one A100 | <0.1 PPL on Wikitext-2/C4 | Research code |
| H2O (20% heavy hitters) | arXiv 2306.14048 [S29] | up to 29× throughput | (not in abstract) | No user flag |
| StreamingLLM (attention sinks) | arXiv 2309.17453, ICLR 2024 [S30] | 22.2× vs sliding-window recompute | Stable perplexity; **does not retain evicted content** | llama.cpp `--context-shift` / mlx `--max-kv-size` are related rotating-window modes. mlx-lm: small values give "worse quality" [S9] |
| SnapKV | arXiv 2404.14469 [S31] | 3.6× speed, 8.2× memory at 16k | "comparable" on 16 long-sequence datasets | No user flag |
| PyramidKV | arXiv 2406.02069 [S32] | keeps ~12% of KV | matches full KV on LongBench | No user flag |
| **Agent-specific evidence** | SideQuest arXiv 2602.22603 [S37]; AgentKV arXiv 2609.14872 [S36] | — | SideQuest: "existing heuristics fail to support multi-step reasoning models effectively". AgentKV: recency-based eviction "systematically undervalues keys that upcoming phases will need"; +5.3 to +5.5 pts over prior eviction methods | **Eviction is a quality risk for agents.** Both are research systems |

### 1.5 Cascades, routing, speculative decoding

| Technique | Source | Savings | Quality | Agent caveat |
|---|---|---|---|---|
| FrugalGPT cascade | arXiv 2305.05176 [S38] | up to 98% at GPT-4 parity | +4% at equal cost | Single-turn QA benchmarks |
| RouteLLM (strong/weak router) | arXiv 2406.18665 [S39] | 3.66× fewer GPT-4 calls at 95% GPT-4 quality on MT-Bench; **only 1.41× (MMLU, 92%) and 1.49× (GSM8K, 87%)** | Quality drops at the savings points on MMLU and GSM8K | Paper assumes "short prompts in a single turn setting" |
| Efficient Agents (component choices for an agent framework) | arXiv 2508.02694 [S43] | $0.398 → $0.228 per task | **96.7%** of OWL on GAIA, a 3.3% relative loss | Agent-level, but bundles many choices |
| OpenRouter Auto Router (`openrouter/auto`) | OpenRouter docs [S40] | no router fee; model chosen by a task classifier ranked on community spend, with `cost_tier` | **No published quality benchmark** | Has session stickiness, which matters because every model switch starts a cold cache |
| TypeSafe Jev Router (`typesafe/jev-router`) | OpenRouter models API [S41] | picks "model and reasoning effort for each request, balancing quality, speed, and cost"; price listed as variable (`-1`) | **No published benchmark, stickiness or caching behavior** | Unverified for tool loops (agent-mode §12 step 14 already says "once its tool-calling behavior is verified") |
| Speculative decoding | arXiv 2211.17192, ICML 2023 [S42] | 2–3× decode speed (T5-XXL) | **Identical output distribution** (exact) | Local latency only, not hosted cost. llama.cpp `--spec-draft-model`, `--spec-type ngram-*` (no draft model needed) [S8]; mlx-lm `--draft-model`, `--num-draft-tokens` (disables batching) [S10] |

### 1.6 Tool-use savings

| Technique | Source | Savings | Quality | Fit |
|---|---|---|---|---|
| Anthropic tool search (`defer_loading`) | Anthropic docs [S46] | ~55k → >85% fewer definition tokens in a 5-server MCP setup | Selection accuracy "degrades once you exceed 30–50 tools" without it (no figure for with/without). Deferred tools keep the cache because they are appended inline | Docs advise plain tool calling for **fewer than 10 tools**. Nexus has 9 |
| RAG-MCP (retrieve tools before prompting) | arXiv 2505.03275 [S44] | >50% prompt tokens | tool-selection accuracy 13.62% → 43.13% | Large MCP catalogs, not 9 tools |
| Fewer tools for small local models | arXiv 2411.15399 [S45] | up to 70% execution time, 40% power | "agentic success rate improvements" (no figure in abstract) | Supports agent-mode §9 "fewer, shorter tools" |
| Keep tools stable; use `tool_choice`/`allowed_tools` instead of removing | OpenAI docs [S2]; Anthropic [S1] | protects cache | exact | Already partly done (`tool_choice: none` on the last round) |
| Delayed, bulk elision of stale tool output | AgentDiet [S12], Anthropic `clear_at_least` [S47] | see §1.2 | −1 to +2 pp (AgentDiet) | Bulk changes amortize the cache rewrite |

## 2. Where nexus stands today

| Aspect | Current behavior | Evidence-based gap |
|---|---|---|
| Anthropic breakpoints | Two explicit: end of system block, and `history.len()-2` when `build_history` runs (`chat.rs:234`) | **The tool loop never moves them.** `run_chat_loop` (`openrouter.rs:1460`) appends assistant/tool messages with `cache_breakpoint: false`. Writes happen only at breakpoints [S1], so every round's new tool output is re-billed at 1× on every later round of the turn. Illustration (our arithmetic, not a measurement): 20 rounds × 2k new tokens ≈ 380k uncached input-token-equivalents now vs ≈ 88k with a moving breakpoint (≈50k of 1.25× writes + 38k of 0.1× reads) |
| Research/swarm loops | No breakpoints (`usage-accounting.md` "Known gap") | Same fix applies |
| OpenRouter cache lane | `prompt_cache_key` only for OpenAI/OpenCode flavors. The comment says OpenRouter drops unknown fields | OpenRouter now documents `session_id` and falls back to `prompt_cache_key` for sticky routing [S3]. Without one, stickiness starts only after the first cache hit |
| Prompt order | `{{datetime}}` in paragraph 2 of the base prompt; frozen per `cache_epoch` (`memory.rs:44`) | Within an epoch: fine. Across sessions/epochs: the base prompt (≈1.7k chars) never matches. Tools precede system on Anthropic, so the tool cache survives there [S1] |
| Duplicate tool results | Byte-identical (tool, args) → one-line note, mirrored on replay (`tool_result_unchanged_note`) | Exact, cache-stable. Matches AgentDiet's "redundant" category [S12] |
| Tool result cap | `MAX_TOOL_RESULT_CHARS = 8_000`, head kept (`tools.rs:3437`) | Lossy, unmeasured. Agent-mode notes that for builds/tests the tail matters |
| Old tool output | Replayed verbatim until compaction | Observation masking halves cost at comparable solve rate on SWE-bench [S11]. Unused here |
| Compaction | At `compact_threshold` = 60% of the model's context (`app/mod.rs:507`); **the digest replaces everything** (`new_through = messages.len()`, no verbatim tail); runs on `memory_model`; bumps the cache epoch | The studied summarizers keep the last 10 turns verbatim [S11]. Naive "compress this" prompting lost 12.5 pp on AppWorld [S13]. On a 1M-context model, 60% means ≈600k tokens per request before compaction, far past the lengths where NoLiMa/Context Rot show degradation [S18][S19] |
| Forced skill (`/skill name …`) | Injected as a system message **before** the conversation tail for one turn only (`chat.rs:139`, `forced_skill.take()`) | The next turn's prefix diverges at that position (a message-cache miss), and the skill text disappears from later turns |
| Thinking replay | `reasoning_content` string on tool-call assistant rows; plain replies replay without reasoning | Anthropic invalidates the message cache when thinking blocks are dropped [S1]. Whether OpenRouter's mapping preserves them is **unverified**; measure it |
| Tool defs | 9 tools, sent every request; ≈12k chars of Rust source (≈2.5–3k tokens, **our estimate**) | Below the 10-tool / 10k-token threshold for tool search [S46]. Cached as prefix on hosted models |
| Local runtimes | `ollama serve` with only `OLLAMA_HOST`; `mlx_lm.server --model`; no KV/context flags (`serve.rs:77`) | Ollama defaults to **4k context under 24 GiB VRAM** and recommends ≥64k for agents [S35]. Nexus tools plus prompt use most of 4k. llama.cpp/mlx-lm reuse prompt caches by default [S8][S10] |

## 3. Mapping to nexus-chat

Ranked by expected savings ÷ quality risk. "Measure" names the check. Most
checks use `usage_log` (cache read/write per request, already normalized per
usage-accounting.md), the planned `nexus eval` (task success, invalid-call
rate, tokens, cost) and the planned trace store (exact request bytes, so
prefix divergence can be diffed).

### Do now (exact or provably output-neutral)

| # | Change | Touches | Expected effect | Quality risk | Measure |
|---|---|---|---|---|---|
| 1 | **Moving Anthropic breakpoint in tool loops.** Prefer top-level `cache_control: {"type":"ephemeral"}` (automatic caching, supported through OpenRouter for Anthropic [S1][S3]), keeping the system-block breakpoint. Do not exceed 4 breakpoints in total (400 error [S1]). Fallback: in `run_chat_loop`, before each `run_stream`, clear the previous tail marker and mark the last message | `app/chat.rs` `mark_cache_breakpoints`; `provider/openrouter.rs` `run_chat_loop`/`run_stream`; `provider/mod.rs` `ChatMessage` serialization | Large on Claude tool loops: the in-turn tail goes from 1× to ≈0.1× (0.05× on 5.5 models) after a one-time 1.25× write. [S4] measured −78% for Sonnet 4.5 with full-context caching | None. Exact reuse. Watch the 4-breakpoint limit and the per-model minimum (512–4,096 tokens) | `/usage` turn-aggregate rate on Claude: request k>1 in a turn should show `cache_read ≈ prompt − last_round`. Golden-payload test asserting one marker on the last message per round |
| 2 | **Send `session_id`** (= `prompt_cache_key_for`) on the OpenRouter flavor, and update the stale comment | `provider/openrouter.rs` `add_prompt_cache_key` | Stickiness to one upstream from request 1, so fewer cold caches when OpenRouter load-balances providers [S3] | None (routing hint). Does not apply when `provider.order` is set | Cache-read share of the 2nd request per session, before vs after |
| 3 | **Same fix for research/swarm sub-agents** (top-level `cache_control` makes this a one-liner per request) | `app/research.rs` `run_searcher` etc., `app/swarm.rs` | Searchers loop up to N tool rounds, so the same quadratic tail as #1 | None | `usage_log` grouped by research stage |
| 4 | **Static-first system prompt**: move `{{datetime}}`, the web-mode clause and the research note to the tail (agent-mode §7 order) | `assets/system-prompt-base.md`, `app/chat.rs` `system_prompt`/`resolved_base_system_prompt` | Cross-session reuse of base prompt + tools on auto-caching providers (OpenAI ≥1,024 tokens [S2]). Small per request; recurring | Low. The model still sees the date; only its position changes. Run an eval on "what's today" style tasks | Cached tokens on the **first** request of a new session |
| 5 | **Forced skill as a tail message** (after the user message) or persisted for the session, instead of a mid-prefix system insert | `app/chat.rs` `build_history` (`forced_skill`) | Removes a full message-cache miss after every `/skill` turn | Low. Arguably a quality fix: today the skill vanishes on the next turn | Cache read on the turn after `/skill`; eval: multi-turn skill task |
| 6 | **Local context sizing**: launch Ollama with `OLLAMA_CONTEXT_LENGTH` from the model profile (≥64k where memory allows [S35]); warn when tools + prompt exceed ≈50% of the served window | `provider/serve.rs` `LocalRuntime::launch`; agent-mode §9 `ModelProfile.context` | No token savings. Prevents silent loss of prompt/tool context (behavior beyond the window is undocumented [S35]) | Removes a quality risk; costs RAM | `ollama ps` allocated context; eval with a local model |
| 7 | **Keep local prefixes byte-stable** (follows from #4; never rewrite earlier messages except in bulk at compaction) | `build_history`, Harmony `sanitize_history` (must be idempotent) | Prefill reuse in llama.cpp (`cache_prompt` default on) and `mlx_lm.server` LRU [S8][S10]. Prefill dominates laptop latency | None | Server-reported cached-prompt counts (`prompt_cache_count` in mlx-lm; llama.cpp timings) |

### Experiment first (lossy or unmeasured; gate on `nexus eval`)

| # | Change | Touches | Expected effect | Quality risk / evidence | Measure |
|---|---|---|---|---|---|
| 8 | **Observation masking in bulk.** When the request crosses a token trigger, replace tool results older than the last K tool rounds with a placeholder in one step (`clear_at_least`-style), then bump the cache epoch. Keep calls, arguments and reasoning. Never mask unchanged-note targets | `app/chat.rs` `build_history`/`append_tool_call_history`, `provider/mod.rs` `seed_tool_result_dedup`, new setting next to `compact_threshold` | ≈50% cost on long tool sessions [S11]; −40 to −60% input with careful delayed reduction [S12] | Mixed: +1.4 to +2.8 pp on 3/5 configs, **−2.0 and −4.0 pp on Qwen3-32B and Gemini Flash-thinking** [S11]; the window needs per-scaffold tuning. Per-request masking would break the cache [S4], hence bulk | `nexus eval` success + cost per successful task, per model tier; cache write/read ratio after each mask event |
| 9 | **Compaction keeps a verbatim tail** (last ~10 turns or N tokens, as in the studied setup) and uses a structured, itemized digest prompt rather than "compress ALL of the above" | `app/compaction.rs` `start_compaction` (`new_through`), `compaction_tail` | Fewer re-reads after compaction; cheaper follow-ups | Naive summarization prompting: AppWorld 56.0 → 43.5 [S13]; summary runs 13–15% longer [S11]; "context collapse" under iterative rewrite [S21]. Today's whole-history digest is the riskiest variant | Eval tasks that span a compaction boundary; turns-to-finish |
| 10 | **Absolute compaction/masking trigger** (e.g. min(60% of window, a fixed token budget)) instead of 60% of 1M | `app/compaction.rs` `maybe_compact`, `context_limit` | Caps per-request input on long-context models | Shorter can be *better* [S17][S18][S19], but compaction itself is lossy (#9). Tune the budget by eval, not by guess | Eval success vs budget sweep |
| 11 | **Tail-aware result capping** for command/test output (keep head + tail), and caps as a fraction of `profile.context` (agent-mode §9) | `tools.rs` `cap_result`, `format_output` | Fewer re-runs to see the error line | Unmeasured; plausibly positive | Retry rate on build/test eval tasks |
| 12 | **1-hour TTL for long, slow sessions** on Anthropic (2× write vs 1.25×) | `provider/mod.rs` breakpoint serialization; a setting | Pays off when the same prefix is re-used after >5 min roughly twice per hour (our arithmetic) | None (exact) | `cache_creation` vs `cache_read` per turn gap |
| 13 | **Local q8_0 KV cache** as a profile option (llama.cpp `-ctk/-ctv q8_0`, Ollama `OLLAMA_KV_CACHE_TYPE`) to afford the larger window from #6 | `provider/serve.rs` launch env/argv; `ModelProfile` | ≈½ KV memory, which buys context | PPL +0.002 [S33]; "usually no noticeable impact" [S34]. **Avoid q4 K**; Qwen-class high-GQA models lose more [S34] | Local eval at f16 vs q8_0 |
| 14 | **Local speculative decoding** (llama.cpp `--spec-type ngram-*` needs no draft model; mlx-lm `--draft-model`) | `provider/serve.rs` | Decode latency (exact outputs [S42]); no hosted-cost effect | None in distribution. mlx draft disables batching [S10] | tokens/s on eval runs |
| 15 | **Utility-model routing stays per job, not per step**: compaction, memory, titles on a cheap `memory_model` (already the case); evaluate Jev Router / Auto Router **per session** only | `app/models.rs`, `resolve_utility_model_backend` | Cheaper background work | Routing evidence is single-turn [S39]; RouteLLM loses quality at its savings points on MMLU/GSM8K. Per-step switching restarts the cache every round. Jev has no published tool-loop benchmark [S41] | Eval: fixed model vs router, cost per success |

### Not worth it (for nexus, now)

| Technique | Why not |
|---|---|
| Token-level prompt compression (LLMLingua family) on agent history | **Measured −16.7 pp on AppWorld** as a history compressor [S13]. Rewrites bytes, so it breaks caching. Adds a compressor model |
| Tool search / tool RAG for the built-in tools | 9 tools, a few thousand tokens, cached as prefix. Anthropic advises plain tool calling under 10 tools [S46]. Revisit if MCP servers bring 30+ tools [S44][S46] |
| KV eviction (H2O / StreamingLLM / SnapKV / PyramidKV / rotating `--max-kv-size`, `--context-shift`) for agent sessions | Not exposed as flags (except rotating windows, documented to hurt quality [S9]). Agent-specific work says heuristics fail on multi-step tasks [S36][S37]. Evicted tool output is gone |
| 2–4-bit KV (KIVI, KVQuant, q4_0) by default | K cache is quantization-sensitive [S33]. Gains matter for serving throughput, not single-user laptops |
| Per-step cascades (small model first, escalate on low confidence) inside a tool loop | Evidence is single-turn. Each escalation pays a cold-cache prefill on the big model. Efficient Agents still lost 3.3% [S43] |
| Retrieval-only memory (Mem0-style) replacing chat history | −6 J points vs full context on LOCOMO [S16]. Fine as an addition (nexus memory already is), not as a replacement |

## 4. How to tell it worked

| Metric | Source in nexus | Target signal |
|---|---|---|
| Cost per **successful** task | `nexus eval` (planned) | Down, with success flat or up. Cost alone is not a win |
| Turn-aggregate cache-read share | `CacheTally`, `/usage` | Up on Claude tool loops after #1 |
| Cache write ÷ read per session | `usage_log.cache_creation_tokens` | Low. A high ratio is the "rebuilt every turn" signature (usage-accounting.md) |
| Prefix divergence position | trace store (planned): diff consecutive requests' bytes | Divergence only at the tail, except at deliberate epoch bumps |
| Turns to finish, invalid-call rate | `nexus eval`, traces | Not up after masking/compaction (the summarization-elongation effect [S11]) |
| Local prefill tokens/s, cached prompt tokens | llama.cpp timings, mlx-lm `prompt_cache_count` | Prefill of only the new suffix |

## 5. Could not verify

- Whether **OpenRouter maps `reasoning_content`** back to Anthropic thinking
  blocks with signatures. If not, every turn after a reasoning tool call may
  miss the message cache. Needs a live measurement.
- Whether OpenRouter passes Anthropic **`context_management`**
  (`clear_tool_uses`, server-side `compact_20260112`) through. The docs pages
  checked do not say.
- **Ollama cross-request KV/prompt reuse**: the FAQ and context-length pages
  do not document it, and neither page says what happens when a prompt
  exceeds the window.
- **Jev Router** pricing, stickiness, caching and tool-loop quality: the model
  listing gives only a one-line description and variable pricing (`-1`).
- **Auto Router** quality: no benchmark published.
- **mlx-lm `--kv-bits`** quality: no owner-published numbers.
- llama.cpp speculative-decoding speedups: the flags are documented, the
  numbers are not, so measure locally.
- The tool-definition token count (≈2.5–3k) is an estimate from source
  length, not a tokenizer count.
- ACON's appendix "Latency and KV-cache dynamics" and the full-paper tables of
  SideQuest/AgentKV were not read past the abstract.
- MemGPT and A-MEM abstracts carry no quality numbers. The A-MEM figures above
  come from its paper body, and the Mem0 paper's own A-Mem measurement
  disagrees with them.
- The *Don't Break the Cache* abstract (41–80%, 13–31%) disagrees with its own
  Table 2 (Gemini −27.8%, TTFT −2.9%). The table is cited above.

## Sources

All accessed 2026-10-15.

[S1]: https://platform.claude.com/docs/en/build-with-claude/prompt-caching
[S2]: https://developers.openai.com/api/docs/guides/prompt-caching
[S3]: https://openrouter.ai/docs/features/prompt-caching
[S4]: https://arxiv.org/abs/2601.06007
[S5]: https://docs.vllm.ai/en/latest/design/prefix_caching.html
[S6]: https://docs.vllm.ai/en/latest/features/automatic_prefix_caching.html
[S7]: https://arxiv.org/abs/2312.07104
[S8]: https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md
[S9]: https://github.com/ml-explore/mlx-lm/blob/main/README.md
[S10]: https://github.com/ml-explore/mlx-lm/blob/main/mlx_lm/server.py
[S11]: https://arxiv.org/abs/2508.21433
[S12]: https://arxiv.org/abs/2509.23586
[S13]: https://arxiv.org/abs/2510.00615
[S14]: https://arxiv.org/abs/2310.08560
[S15]: https://arxiv.org/abs/2502.12110
[S16]: https://arxiv.org/abs/2504.19413
[S17]: https://arxiv.org/abs/2307.03172
[S18]: https://www.trychroma.com/research/context-rot
[S19]: https://arxiv.org/abs/2502.05167
[S20]: https://arxiv.org/abs/2505.06120
[S21]: https://arxiv.org/abs/2510.04618
[S22]: https://arxiv.org/abs/2310.05736
[S23]: https://arxiv.org/abs/2310.06839
[S24]: https://arxiv.org/abs/2403.12968
[S25]: https://arxiv.org/abs/2310.06201
[S26]: https://arxiv.org/abs/2407.08892
[S27]: https://arxiv.org/abs/2402.02750
[S28]: https://arxiv.org/abs/2401.18079
[S29]: https://arxiv.org/abs/2306.14048
[S30]: https://arxiv.org/abs/2309.17453
[S31]: https://arxiv.org/abs/2404.14469
[S32]: https://arxiv.org/abs/2406.02069
[S33]: https://github.com/ggml-org/llama.cpp/pull/7412
[S34]: https://docs.ollama.com/faq
[S35]: https://docs.ollama.com/context-length
[S36]: https://arxiv.org/abs/2609.14872
[S37]: https://arxiv.org/abs/2602.22603
[S38]: https://arxiv.org/abs/2305.05176
[S39]: https://arxiv.org/abs/2406.18665
[S40]: https://openrouter.ai/docs/guides/routing/routers/auto-router
[S41]: https://openrouter.ai/api/v1/models
[S42]: https://arxiv.org/abs/2211.17192
[S43]: https://arxiv.org/abs/2508.02694
[S44]: https://arxiv.org/abs/2505.03275
[S45]: https://arxiv.org/abs/2411.15399
[S46]: https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool
[S47]: https://platform.claude.com/docs/en/build-with-claude/context-editing
[S48]: https://arxiv.org/abs/2309.06180

- **S1:** pricing multipliers, the 4-breakpoint limit, the 20-position
  lookback, writes only at breakpoints, the invalidation table, top-level
  automatic caching, and TTL measured from request start.
- **S4:** HTML v2 read for Table 2 and the strategy definitions.
- **S5–S8, S48:** prefix caching / KV sharing at the runtime level.
  PagedAttention (S48): 2–4× throughput at the same latency (abstract).
- **S10:** read from source (`LRUPromptCache`, `--prompt-cache-size` default
  10, `--kv-bits`, `--draft-model`; a draft model makes the server
  non-batchable) and from
  [SERVER.md](https://github.com/ml-explore/mlx-lm/blob/main/mlx_lm/SERVER.md).
- **S11:** HTML v3 Tables 1–2 and the hybrid section. **S12:** HTML v2 Table 4.
  **S13:** HTML v3 main table (LLMLingua/FIFO/Retrieval baselines) and the
  compressor-cost section. **S15/S16:** HTML tables.
- **S18:** a vendor technical report (Chroma), not peer-reviewed. Cited for
  direction, not for numbers.
- **S33:** a research/measurement PR by a llama.cpp maintainer. The model is
  unnamed; superseded by #7527 for the FA implementation.
- **S41:** `typesafe/jev-router` and `openrouter/auto` entries from the live
  models endpoint (`pricing: -1`, i.e. variable). The model page
  (https://openrouter.ai/typesafe/jev-router) only documents accepted
  parameters.
