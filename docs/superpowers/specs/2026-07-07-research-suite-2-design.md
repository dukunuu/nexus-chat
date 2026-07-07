# Research Suite 2 — design

Date: 2026-07-07. Follow-up to `2026-07-06-research-suite-design.md` (shipped).
Four areas, all approved: deeper sources, steer + verify, standing research, report export.

## 1. Deeper sources

All changes live in `src/tools.rs` and flow through the existing `web_cache`
write-through cache, so every new content type is cached and searchable like
HTML pages.

### 1.1 PDF reading

`fetch_url_text` inspects the response `Content-Type`. When it is
`application/pdf` (or the URL ends in `.pdf` and the body starts with
`%PDF`), extract text with the `pdf-extract` crate (pure Rust — no system
dependency) instead of the HTML pipeline. Extraction failures degrade to an
explanatory `[could not extract PDF text: …]` result, never an error that
kills the searcher.

Unlocks: arXiv full papers, whitepapers, government reports.

### 1.2 HTML tables as markdown

The HTML→text conversion currently flattens `<table>` content into prose,
destroying benchmark/spec/pricing data. Change: render each `<table>` as a
GitHub-style pipe table (`| a | b |` rows with a `---` separator after the
header row). Nested tables degrade to flattened text (no recursion).

### 1.3 YouTube transcripts

When `fetch_url` targets a `youtube.com/watch?v=…` or `youtu.be/…` URL,
fetch the transcript via YouTube's timedtext endpoint (no API key): scrape
the watch page for the caption track URL, fetch it, strip XML tags, join the
cue text. If no caption track exists, fall back to the normal page scrape.

### 1.4 HN/Reddit discussion mining

New research-only tool `discussion_search(query)`:

- Hacker News via the Algolia API
  (`https://hn.algolia.com/api/v1/search?query=…&tags=story`), top stories by
  relevance with points/comment counts and story URLs.
- Reddit via `https://www.reddit.com/search.json?q=…&sort=relevance`, top
  posts with subreddit, score, and permalink.

Result format mirrors `web_search`: one source line per hit so the existing
dedup/citation machinery applies. Added to the research-only allowlist in
`ToolBox::defs()`/`run()` alongside `web_search`/`fetch_url`/`academic_search`.
Responses cached in `web_cache` keyed by the request URL.

## 2. Steer + verify

### 2.1 Mid-flight steering (`/steer`)

While research runs, `/steer <text>` queues an extra instruction. Mechanism:

- `App` gains `research_steer_tx: Option<mpsc::UnboundedSender<String>>`,
  populated by `start_research_with_gate`, cleared when the pipeline
  finishes.
- The pipeline holds the matching receiver. At each round boundary (after the
  round-1 searchers return; again after the gap round), it drains the queue
  non-blockingly. Any queued steers become an extra searcher round (one
  sub-question per steer), whose findings join the pool before synthesis /
  re-synthesis.
- `/steer` with no active research → status message, no-op.
- Each accepted steer posts a stage row ("steer: …") so the user sees it was
  picked up.

Rejected alternative: plain typing during research counts as a steer —
ambiguous with normal chat; a session can receive ordinary messages while
research runs in another session.

### 2.2 Pin / discard sources

From the citation/source selection in history (the same selection that
powers `o` = open):

- `x` — discard: the source's domain is added to a **session-scoped**
  blocklist. Later searcher rounds exclude it (same mechanism as the global
  `blocked_domains`: query rewriting + fetch guard), and the Writer is
  instructed to drop citations to it.
- `p` — pin: the Synthesizer/Writer prompts list pinned URLs as "prioritize
  these sources".

Storage: `session_sources` gains a `flag` column (`NULL` | `'pinned'` |
`'discarded'`) via the migrate-on-open pattern. `ToolBox` reads the
session's discarded domains alongside the global blocklist.

### 2.3 Claim confidence

The Verifier prompt is extended: for each claim in the report, judge
confidence from citation count and cross-source agreement, and tag
low/medium claims inline as `‹low›` / `‹med›` immediately after the claim's
citations (high confidence is unmarked — it is the default and tagging it
would be noise). `src/ui/history.rs` styles `‹low›` yellow and `‹med›` dim;
`src/citations.rs`-style pure parsing lives in a small helper with unit
tests.

### 2.4 Quote checking

The Verifier stage currently runs without tools. Change: it runs with a
cache-only toolbox — `fetch_url` restricted to serving from `web_cache`
(a `cache_only: bool` on `ToolBox`; on cache miss it returns
`[not cached]` instead of hitting the network). The Verifier prompt
instructs it to look up each direct quote in the cached source page and
flag mismatches as `‹unverified quote›`, rendered like `‹low›`.

## 3. Standing research (watches)

Zero-infra, on-open model. No daemon.

- New table:
  `watches (id TEXT PK, space_id TEXT, topic TEXT, interval_hours INTEGER,
  session_id TEXT, last_run_at TEXT)`.
- `/watch <topic>` — creates a watch (interval fixed at 24h for now; a
  per-watch interval setter is out of scope) plus a dedicated research
  session it will keep reporting into.
- `/watch` — opens a small picker listing watches (topic, interval, last
  run); `d` deletes, Enter jumps to the watch's session.
- On app startup, after the usual init: any watch with
  `now - last_run_at >= interval` re-runs research (ungated — no plan
  approval prompt for background runs) into its session. Completion lights
  the existing `unread` badge on that session.
- Diff: after the new report is written, one extra `complete` call with the
  previous report + new report → a short "What changed since last run"
  section prepended to the new report message. Newly-seen sources are
  computed as a URL diff against the session's existing `citations` rows and
  listed in that section.
- First run of a watch has no previous report → no diff section.

## 4. Report export (`/export`)

On a research session, `/export` writes
`<space_dir>/reports/<session-slug>.md`:

- The latest research report message body.
- A `## Sources` bibliography generated from the session's `citations` rows
  (numbered, title + URL).
- The "What changed" section, if present (watch sessions).

Re-running `/export` overwrites the same file — the file is a living
document that always reflects the latest run. Status line shows the written
path. `/export` on a non-research session or one with no report → status
message, no-op.

Out of scope (add when asked): PDF export (pandoc/typst dependency), true
daemon scheduling, per-claim drill-down UI, paywalled-source handling.

## Testing

Follows the repo convention: pure functions get unit tests (PDF magic-byte
detection, table→markdown rendering, timedtext XML stripping, discussion
result formatting, steer-queue drain logic, confidence-tag parsing/styling,
watch due-ness computation, diff-section assembly, export file assembly);
network paths (real PDF fetch, YouTube, Algolia/Reddit, background watch
runs) are exercised manually.
