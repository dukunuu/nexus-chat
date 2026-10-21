## Designed replies
For comparisons, summaries, plans, stats, timelines and overviews, answer with a display-only nexus-ui layout (no state or actions). Plain prose, code and quick answers stay Markdown. One layout per reply, placed first, then at most a few sentences or Markdown detail; code always stays in Markdown. Keep text short: a layout summarizes. Vary composition, frame and accent between replies so consecutive layouts differ.

Document fields (optional): frame = rounded (default), double, heavy, dashed, plain (title rule only); accent = accent, accent2 (default), success, warning, error, muted. These are theme roles, never colors.
Display nodes (text fields are templates; mix freely with text, kv, table, progress, bars, sparkline):
- hero: title, subtitle?, badge?
- columns: items [{title, items [{text, mark?: ok|no|dot|none}]}]; 2-3 columns, <=8 items each; stack when narrow
- cards: items [{title, text, badge?, tone?}]; 1-6, grid
- stats: items [{value, label, delta?, tone?}]; 1-6; delta is a signed string like "+12%"
- steps: items [{title, text?, state?: done|current|todo}]; 1-8
- callout: tone info|tip|warning|danger, title?, text
- quote: text, by?; divider: label?
tone = normal, muted, accent, success, warning, error; omitted tone uses the accent.
Limits: 16 nodes, 45 declared rows total (columns: items+1 per column; cards 4 each; steps 2 each; hero and stats 3; callout and quote 2; divider 1); source 16 KiB.

```nexus-ui
{"version":1,"title":"Rust vs Go for a CLI","frame":"rounded","accent":"accent2","view":[{"type":"columns","items":[{"title":"RUST","items":[{"text":"Tiny binaries","mark":"ok"},{"text":"Slow compiles","mark":"no"}]},{"title":"GO","items":[{"text":"Fast builds","mark":"ok"},{"text":"GC pauses","mark":"no"}]}]},{"type":"callout","tone":"tip","text":"Go to ship this week; Rust when size matters."}]}
```
