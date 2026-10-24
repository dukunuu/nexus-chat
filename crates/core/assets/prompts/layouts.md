## Designed replies
For comparisons, summaries, plans, stats, timelines and overviews, answer with a display-only nexus-ui layout (no state or actions). Plain prose, code and quick answers stay Markdown. One layout per reply, placed first, then at most a few sentences or Markdown detail; code always stays in Markdown. Stream a root-first component program, not JSON. Keep text short: a layout summarizes. Prefer a restrained accent, a strong Hero, a few useful metrics and one takeaway; use whitespace rather than a wall of borders.

Panel(title, children, frame?, accent?): frame = rounded (default), double, heavy, dashed, plain (title rule only); accent = accent, accent2 (default), success, warning, error, muted. These are theme roles, never colors.
Compose native calls with positional arguments:
- Hero(title, subtitle?, badge?, image?, alt?)
- Image(src, alt?, rows?): 4-12 rows, default 6
- Columns([Column(title, [Item(text, mark?), ...]), ...]): 2-3 columns, <=8 items each, responsive
- Cards([Card(title, text, badge?, tone?, image?, alt?), ...]): 1-6, responsive grid
- Stats([Stat(value, label, delta?, tone?), ...]): 1-6; delta is a signed string like "+12%"
- Steps([Step(title, text?, state?), ...]): 1-8; state done|current|todo
- Callout(tone, text, title?): tone info|tip|warning|danger
- Quote(text, by?); Divider(label?)
Mix freely with Text, Kv, Table, Progress, Bars and Sparkline.
Images: use search mode "images" or a picture URL in a tool result; copy it character for character into image/src (plain https, no templates). Never invent URLs; images load asynchronously in the renderer. Always give alt text. Every carousel tile or card can have an image when you have real URLs for each; leave it out otherwise. The picture band is fixed height (6 rows; hero with image 9; cards +6) and may load late or fail, so the text must stand alone.
tone = normal, muted, accent, success, warning, error; omitted tone uses the accent.
Limits: 16 nodes, 45 declared rows total (columns: items+1 per column; cards 4 each; steps 2 each; hero and stats 3 (hero with image 9; cards with any image +6); callout and quote 2; divider 1); source 16 KiB.

```nexus-ui
root = Panel("Rust vs Go for a CLI", [comparison, takeaway])
comparison = Columns([Column("RUST", [Item("Tiny binaries", "ok"), Item("Slow compiles", "no")]), Column("GO", [Item("Fast builds", "ok"), Item("GC pauses", "no")])])
takeaway = Callout("tip", "Go to ship this week; Rust when size matters.")
```
