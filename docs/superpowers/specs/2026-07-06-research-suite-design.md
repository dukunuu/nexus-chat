# Research Suite — Design

Date: 2026-07-06
Status: approved

## Problem

Deep research (`/research`) landed, but nexus-chat still trails Perplexity as a daily
research tool: normal chat answers aren't web-grounded or cited by default, the research
pipeline is a black box the user can't steer, retrieval quality is basic (no freshness,
no domain control, no scholarly sources, duplicate sources reach synthesis), and nothing
learned persists — every research run starts from zero.

## Goal

Four coordinated upgrades, implemented in one pass:

1. **Web answer mode** — per-session toggle forcing search-first, inline-cited answers.
2. **Research UX** — plan approval gate, live activity feed, source-aware follow-ups,
   first-class citations.
3. **Source quality** — freshness + domain filters, cross-searcher dedup/diversity,
   academic search backend.
4. **Knowledge persistence** — fetched-page cache, local-first planning, per-space
   citation index.

## Design

### 1. Web answer mode

- New per-session bool `web_mode`, toggled by `/web`, shown in the status line as
  `🌐 web`. Persisted as a column on the sessions table (default 0).
- When on, the system prompt for each turn appends an instruction block: the model MUST
  call `web_search` before answering, cite claims inline as `[n]`, and end with a
  `Sources:` list mapping n → URL. The block includes today's date.
- No new orchestration: the existing tool loop does the work; this is prompt + toggle
  plumbing only. When off, behavior is exactly today's.

### 2. Research UX

**Plan approval gate.** After the Planner stage, the pipeline pauses and posts the
sub-questions as a `research_plan` message in the research session. Status line offers
`[e]dit / [Enter] continue`. Edit opens the input textarea prefilled with the questions,
one per line; submitting replaces the plan. A 60s timeout auto-continues so a
backgrounded session never hangs. `/research! <topic>` skips the gate entirely.
The pause/resume is a `tokio::sync::oneshot` the pipeline awaits with
`tokio::time::timeout`; the app holds the sender keyed by session id.

**Live activity feed.** `ResearchUpdate::Stage(String)` becomes structured:

```rust
enum ResearchUpdate {
    Stage { label: String, detail: String }, // detail: searcher idx, sub-question, url, source count
    Done(Result<String, String>),
}
```

Each named stage keeps ONE `research_stage` row that is updated in place (DB update +
redraw) rather than appending a new row per event. Searchers report fetch/search events
through the existing `AppEvent::Research` channel.

**Follow-up drill-down.** The gathered searcher findings for a research session are
persisted as that session's *source bundle* (backed by the web cache, §4, plus a
`session_sources(session_id, url_norm)` join table). Follow-up turns in a research
session expose a `search_sources(query)` tool that substring/keyword-searches the
bundle's cached texts; a system note tells the model to prefer it over `web_search`
and only go to the web on a miss.

**First-class citations.** In rendered markdown, `[n]` tokens are styled with the theme
accent color. The trailing `Sources` section of a message is parsed into (n, url) pairs.
In message-selection mode, `o` opens the citation under selection (or prompts for n)
via the `open` crate. Parsing is a pure function, unit-tested.

### 3. Source quality

**Freshness.** `web_search` gains optional `recency` param: `day|week|month|year`.
Mapped to LangSearch `freshness` and SearXNG `time_range`; DuckDuckGo ignores it (noted
in the tool description). Searcher and web-mode prompts state today's date.

**Domain filters.** Optional `include_domains` / `exclude_domains` array params on
`web_search`, implemented backend-agnostically by rewriting the query with `site:` /
`-site:` terms. New per-space setting `blocked_domains` (comma-separated, editable in
settings popup) is always applied as excludes.

**Dedup + diversity.** Before the Synthesizer stage, source URLs across all searcher
findings are normalized (lowercase host, strip `utm_*`/`fbclid` params, strip trailing
slash and fragment) and duplicate-source findings lines are collapsed. Searcher prompt
gains: "prefer sources from domains not already cited in your findings." Normalization
and dedup are pure functions, unit-tested.

**Academic backend.** New tool `academic_search(query, limit=10)` calling the Semantic
Scholar Graph API (free, keyless): returns title, authors, year, venue, abstract
snippet, citationCount, and URL per paper. Available to interactive chat and research
searchers. The Planner prompt mentions it for scholarly topics. HTTP 429 returns the
error as tool text; the model falls back to `web_search`.
<!-- ponytail: Semantic Scholar only; add arXiv/Crossref backends if coverage gaps show up -->

### 4. Knowledge persistence

**Source cache.** New table:

```sql
CREATE TABLE web_cache (
  url_norm   TEXT PRIMARY KEY,
  url        TEXT NOT NULL,
  title      TEXT,
  text       TEXT NOT NULL,
  fetched_at TEXT NOT NULL
);
```

`fetch_url` writes through on success and serves from cache when `fetched_at` is under
24h old; optional `fresh: true` param bypasses. Global (not per-space): page text is
space-agnostic.

**Local-first research.** Before the Planner runs, if an embedding model is configured,
the topic is embedded and top-k chunks from the space's files are retrieved (existing
embedding search). They're injected into the Planner prompt as "already known — plan
sub-questions for the gaps." Silently skipped when embeddings are unconfigured or the
space has no files.

**Citation index.** New table `citations(space_id, report_file, url, title)`, populated
by parsing the final report's `## Sources` section when it is saved. New tool
`list_citations(query?)` returns rows whose url/title/report match the substring (all
rows when omitted), so "which reports cite nature.com" and "what have we researched
about X" work.
<!-- ponytail: substring match over citations; claim-level indexing only if this proves too coarse -->

## Error handling & bounds

- Plan gate: timeout guarantees forward progress; an edit submitted after timeout is a
  no-op with a status message.
- Web cache read/write failures degrade to a live fetch — never block a tool call.
- Academic search errors surface as tool result text, not pipeline failure.
- All new parsing/normalization (citations, source sections, URL normalization, plan
  edits) are pure functions with unit tests, matching the repo's existing convention of
  testing pure logic and manually exercising network paths.
- Existing research bounds unchanged (6 sub-questions, 2 rounds, searcher iter cap).

## Implementation order

1. Source-quality tool params + URL normalization/dedup (self-contained in `tools.rs`)
2. Web cache table + `fetch_url` write-through/read path
3. Web answer mode toggle
4. Citation rendering + open-keybind
5. `academic_search` tool
6. Local-first planner input + citation index
7. Plan gate, live feed, drill-down (deepest changes to `research.rs`/app plumbing)

## Testing

Unit tests for every pure function listed above; DB tests for `web_cache`, `citations`,
`session_sources` alongside existing `db.rs` tests; a research-pipeline test covering
the plan-gate timeout path with a mocked provider, following the pattern of existing
research tests in `app/tests.rs`. Network-calling paths (Semantic Scholar, real
fetches) exercised manually, per repo convention.
