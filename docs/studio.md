# Design: nexus studio — the canvas of the conversation (decks first, then docs and mini-apps)

Status: the core of D1 is implemented: deck format **version 2** (a pixel node
tree with Figma-style auto-layout, numeric typography, named text styles and
imported fonts), a `taffy` layout engine with browser parity, a v2 `deck` tool,
and a studio backend and UI that speak v2. D1 editor controls now include
auto-layout wrapping and reparenting, smart guides, align/distribute, scrub
fields, font picking/import and a drag-to-reparent layers tree; details and
remaining limitations are under *Editor status*. D2 adds typed slide transitions and node builds (schema, validation, three
operations), an Animate panel and step-based presentation playback; see
*Animations*. Terminal-theme identity and revised comments are implemented.
Supersedes the editor half of `docs/web-editors.md`; that
document's device-key pairing stays the plan for remote access (phase S5).

## Decision

Studio has **no chat room of its own**. The conversation stays in nexus (the
TUI today, the web app later), and studio is the **canvas** of that
conversation: an extension of it, not a second place to talk.

- When the user asks for a change, the model **edits the deck directly** with
  the `deck` tool. The change shows up live in studio and is undoable there.
- When the model is reviewing, or unsure what the author wants, it **pins
  comments** on slides and blocks, each optionally carrying a suggested edit
  with Preview, Apply and Dismiss.
- The model **sees slides as images**: the core renderer draws a deck to PNGs
  and the `deck` tool returns them as image references that the chat loop
  turns into vision input.
- The user comments from studio (Send to nexus) and the comment lands in the
  conversation as a user message with the slide image attached.

So: `/studio` in the TUI opens **nexus studio**, a native desktop window that
ships with nexus and is linked to that conversation. `nexus studio [path]` also
opens it standalone, where it is a plain deck editor that says it is not linked
to a conversation.

## Shape

| Piece | Where | Notes |
|---|---|---|
| Deck schema, validation, operations, revision hashes | `crates/core/src/deck.rs` and `deck/{model,validate,migrate,ops}.rs` | pure data; shared by studio, the `deck` tool and the renderer; no UI deps |
| Layout | `crates/core/src/deck/layout.rs` (`taffy`) and `deck/text.rs` | node rectangles in px; text measured with the renderer's fonts |
| Renderer | `crates/core/src/deck/render.rs` | deck → SVG → PNG with `resvg`; one slide, a numbered contact sheet, and a revision-keyed cache under `<space files>/decks/.renders/` |
| `deck` tool | `crates/core/src/tools/deck.rs`, spec in `tools/spec.rs` | create, read, view, edit, comment, resolve; confined to `<space files>/decks/` |
| Comment threads | `crates/core/src/deck/feedback.rs` | `<deck>.comments.json`, shared by the tool and studio |
| Link protocol | `crates/core/src/deck/link.rs` | versioned JSON lines; types in core so the web app can reuse them |
| Terminal side of the link | `crates/tui/src/studio.rs`, `/studio` | spawns studio, turns comments into messages, tells it what the model changed |
| Desktop app | `crates/studio` (package `nexus-studio`, `publish = false` until S3) | `tao` window + `wry` webview; the Rust backend owns the deck file |
| Studio UI | `crates/studio/ui/` | HTML/CSS/vanilla JS modules embedded with `include_str!`; no Node build step |
| Launcher | `nexus studio [path]` in `crates/tui/src/cli.rs` | runs `nexus-studio` from PATH or beside the `nexus` binary |

### Terminal identity and comments

Studio's **chrome follows the nexus terminal palette**, independently of the
OS light/dark preference. `crates/core/src/theme_colors.rs` owns the existing
omarchy/Alacritty and Ghostty parsing and shade derivation; the TUI converts
explicit RGB roles to ratatui colors while retaining its ANSI fallbacks.
Studio receives plain hex roles through `Theme { colors }` on attach and theme
changes. Standalone studio loads and polls the same theme sources. Where the
terminal uses an unspecified ANSI color, studio uses its xterm equivalent;
custom terminal remappings of ANSI colors cannot be inferred from theme files.
Chrome uses system sans, monospace metadata and shortcuts, `accent` for you and
interactive controls, and `accent2` with the TUI's `✦` glyph for nexus. Deck
colors and font choices remain separate from these chrome tokens.

Comment pins are circular, constant-size 24 px badges. A severity ring uses
error / accent2 / dim for must / should / consider. Each pin is outside the
anchored layer, at the nearest available corner, with a 1 px leader to the
anchor; placement avoids other layer boxes and text overflow too. Free-point
pins start above/right of the click. Hover enlarges the badge and shows a
one-line preview. Comment mode uses a crosshair-plus cursor, not a badge
following the pointer.

Threads render code spans, bold and italic with DOM nodes. Links are inert
text; no comment text is interpreted as HTML. Edit titles summarize the actual
operations using the guarded pre-edit values, even when an old sidecar's
`why` is just "Suggested edit". Font edits show the real px values (`size 42 → 58 px`),
read from the node's overrides or its text style. Applied cards say
"Applied · undo with Cmd+Z"; the backend still saves an application as one undo
step.

Canvas navigation supports Ctrl/Cmd+scroll (and native webview pinch) to zoom
around the pointer, Space+drag or two-axis scroll to pan, Shift+1 to fit and
reset pan, and Shift+0 for native 100%. Numeric zoom remains fixed while the
window resizes; fit follows the available canvas. Native 100% is the slide's own
1920 px width.

Why a webview rather than drawing natively: slides are layout plus type, and
HTML/CSS does both well. `wry` uses the system webview (WKWebView on macOS,
WebView2 on Windows, WebKitGTK on Linux), so nothing heavy is bundled. Linux CI
and installs need `libwebkit2gtk-4.1-dev`.

### The file is the single source of truth

The deck file is shared by the user's studio window and the model's tool. There
is no sync protocol for it:

- **Autosave.** Every edit studio applies is written to the deck file at once
  (a sibling temp file, then a rename, so nobody reads a half-written deck).
  There is no unsaved state and no Save button.
- **The model's edits.** The `deck` tool writes the same file the same way.
  Studio polls the deck and `<deck>.comments.json` every 500 ms (modified time
  and length, then the revision). A changed deck is reloaded as **one undo
  step**: the previous deck goes on the undo stack, so Cmd+Z takes the model's
  whole edit back, and redo restores it. Undo is written to the file too, so
  the model sees it on its next `read`.
- **The changed blocks flash** briefly, and a `Focus` from nexus jumps to the
  changed slide.
- **Conflicts.** If an edit made in the window races an external change, the
  file wins and the toast says so ("the deck changed on disk first, so your
  last action was not applied").
- Comment threads work the same way, except a comments-only change never
  drops a local action.

### Boundary between UI and backend

The webview UI is untrusted presentation. It reaches the backend only through
one IPC channel (`window.ipc.postMessage` → a typed JSON command enum in Rust),
and the backend answers with events. The UI never gets file paths outside the
opened deck's folder, and has no network access: a strict CSP
(`default-src 'self'; img-src 'self' data: nexus:`) plus a custom `nexus://`
protocol that serves only the embedded UI and the deck's own assets. Remote
images in decks go through `nexus_core::remote_image` (the same vetted fetcher
as the TUI) and reach the page as `nexus://asset/<hash>`. The link carries no
secrets, and studio has no host token and no host connection.

## Deck format (`*.deck.json`, version 2)

A slide is a **frame** of the theme's size (1920 × 1080 px) holding a tree of
**nodes**. Every length is a pixel.

```json
{
  "version": 2,
  "title": "Q3 review",
  "theme": {
    "colors": {"bg": "#0f1115", "fg": "#e8e6e3", "accent": "#7aa2f7", "muted": "#8b8f98"},
    "fonts": {"heading": "Brand Sans", "body": "Inter", "mono": "JetBrains Mono"},
    "size": {"w": 1920, "h": 1080},
    "text_styles": {"title": {"family": "heading", "size": 120, "weight": 700, "line_height": 1.08}},
    "font_files": [{"family": "Brand Sans", "file": "assets/fonts/brand.ttf", "weight": 700, "fallback": ["Inter"]}]
  },
  "slides": [{
    "id": "s1", "notes": "Open with the headline number.",
    "layout": {"mode": "column", "gap": 32, "padding": [160, 160, 160, 160], "main": "center", "cross": "stretch"},
    "transition": {},
    "children": [
      {"id": "b1", "kind": "text", "text": "# Q3 review", "style": "title"},
      {"id": "row", "kind": "frame", "layout": {"mode": "row", "gap": 64, "cross": "stretch"},
       "fill": "muted", "radius": 24, "sizing": {"h": 420}, "children": [
        {"id": "b2", "kind": "text", "text": "Revenue up **18%**", "sizing": {"w": "fill"},
         "font": {"size": 58, "color": "accent"}},
        {"id": "b3", "kind": "image", "src": "assets/chart.png", "fit": "cover", "sizing": {"w": 640, "h": "fill"},
         "position": {"x": 1000, "y": 40}}
      ]}
    ]
  }]
}
```

- **Container** (slide or frame): `layout {mode: none|row|column, wrap, gap,
  gap_cross, padding [t,r,b,l], main: start|center|end|space_between, cross:
  start|center|end|stretch}`, `children`. In `mode: none` every child is
  absolute. A frame adds `fill`, `stroke {color, width}`, `radius`, `clip`; a
  slide adds `fill`, `notes`, `name`, `transition` and `builds`.
- **Node**: `id`, `name?`, `kind`, `sizing {w, h: px | "hug" | "fill", min_w,
  max_w, min_h, max_h}`, `position: "auto" | {x, y}` (absolute: ignore
  auto-layout), `opacity`, `rotation`, `hidden`, `locked` and an `animation` (played in
  present mode only). Kinds: `frame`, `text`, `image`, `shape`, `table`, `code`,
  `chart`.
- **Typography.** `text`, `table` and `code` nodes name a theme **text style**
  (`title`, `heading`, `subheading`, `body`, `caption`, `code`, or one added with
  `set_text_style`) and override fields with `font {family, size px, weight
  100–900, italic, line_height (multiplier), letter_spacing (em), color, align,
  valign, transform}`. Text hugs its width, wraps inside a fixed or `fill`
  width with `hug` height, or sits in a fixed box. `family` is a theme role
  (`heading`, `body`, `mono`), an installed family or a deck font.
- **Fonts.** The theme's `font_files` register files under `assets/fonts/`
  (`ttf`, `otf`, `ttc`, `woff2`) as families, with `fallback` families. The
  core renderer loads `ttf`, `otf` and `ttc`; a `woff2` file is for the studio
  webview and renders with its fallback in PNGs.
- **Colors** are theme roles or hex; **images** are HTTPS or relative to the
  deck's `assets/`; text is a Markdown subset (headings, lists, bold, italic,
  inline code, links).
- **Ids** are stable strings unique within the deck; a **revision** is the
  sha256 of the canonical JSON. Validation repairs missing or duplicate ids and
  fills missing theme tokens and text styles; every other error names a JSON
  path (`$.slides[1].children[0].font.size`). Limits: 200 slides, 200 nodes per
  slide, frames nested 8 deep, 8 KB of text per node.

### Animations

Transitions belong to the **incoming** slide:

```json
{"id":"s2","transition":{"type":"smart","duration_ms":600,"easing":"ease_out"},"builds":["headline","evidence"],"children":[{"id":"headline","name":"Headline","kind":"text","text":"Ideas in motion","animation":{"build_in":{"effect":"fade","trigger":"click"}}},{"id":"evidence","kind":"text","text":"- One source\n- A connection","animation":{"build_in":{"effect":"rise","trigger":"with_previous","by":"item","stagger_ms":80}}}]}
```

- `transition`: `type` is `none|fade|push|slide|cover|zoom|smart`, optional
  `direction` is `left|right|up|down`, `duration_ms` is 0–5000 (default 400),
  `easing` is `linear|ease|ease_in|ease_out|ease_in_out|spring` (default
  `ease_out`). Spring uses a bounded cubic-bezier overshoot. `direction` on a
  push, slide or cover is the way the content moves (`left` brings the new
  slide in from the right); default `left`.
- `animation` holds optional `build_in`, `emphasis`, `build_out` slots.
  Each Build has `effect`, optional `direction`, `duration_ms` (0–5000,
  default 400), `delay_ms` (0–10000, default 0), `easing`, `trigger`
  (`click|with_previous|after_previous`, default click), `by`
  (`whole|word|line|item`, default whole), optional `stagger_ms` (0–10000,
  playback default 60). Entry/exit effects: `appear|fade|rise|fly|scale|wipe|blur|
  typewriter`; emphasis: `pulse|shake`. Typewriter and non-whole segmentation
  require text; list items are Markdown text. Word/visual-line/item spans are
  generated only in playback DOM, retaining inline formatting, never saved.
- `builds:[node_id...]` orders animated nodes. Missing animated nodes append
  depth-first; unknown, duplicate and no-longer-animated references are removed
  on load, the way other dangling references are repaired. Each node runs its slots in entry → emphasis → exit order. Dragging
  a row reorders the node and its slots together.
- **Typed schema.** `Slide.transition` and `Node.animation` are strongly typed
  (`Transition`, `Animation`, `Build` in `deck/model.rs`; unknown fields are
  rejected). `Deck::check` range-checks every value and the slot/effect/kind
  rules with located errors (`$.slides[1].children[0].animation.build_in.effect`).
  **Migration:** a transition or animation that does not parse (the old free-form
  placeholders such as `{"kind":"push"}`, a typo, a wrong type) is dropped rather
  than failing the deck, and each drop is reported as a load note: studio shows it
  once as a notice, the `deck` tool's `read` prints `load note:` lines. Edits are
  stricter: through `apply`, unparseable animation data is an error, so a
  merge patch cannot silently lose it.
- Operations: `set_transition{slide,transition|null}`,
  `set_animation{id,slot,build|null}`, `reorder_builds{slide,ids}` (every id must
  be an animated node of that slide, listed once). Payload errors are located like
  any other deck error and reject the whole transaction. Each editor edit/scrub/drop
  is one `Apply` and one undo snapshot; previews never write. Merge patches on
  `update_node`/`update_slide` continue to merge animation objects.

**Editor.** The design sidebar has a Design and an Animate tab. Animate shows the
slide's transition (picker, direction, duration scrub field, easing), the
selected layer's build in / emphasis / build out rows (effect, start trigger,
duration, delay, direction, easing, and for text by/stagger, with a Preview that
plays on the canvas and never writes) and the slide's build order: numbered rows
(step, trigger icon, effect, layer name), click to select, drag to reorder.
While the tab is open, numbered badges sit just outside each animated layer on the
canvas, like comment pins. A slide with a transition shows a small glyph beside its
rail thumbnail, and hovering the thumbnail plays the transition there for one
second. Play from here (Shift+Enter or the button) presents from the current slide
and step. Every control is one set_transition / set_animation / reorder_builds
operation, so one undo.

**Playback.** Click, Space or Right runs the next click-triggered build and
following with/after-previous entries. With-previous starts relative to the
previous start; after-previous starts relative to its completion (including
stagger). Each build's delay is added. Leading non-click builds play on entry.
Once click groups are exhausted, the next advance enters the following slide.
Back/Backspace instantly reconstructs the previous click boundary, crossing to
previous slides at their final step. Home/End go to the first/last boundary;
number + Enter jumps to a slide's initial boundary; Esc exits. A new advance
settles unfinished playback before running its next group. The indicator counts
completed click groups. P toggles current slide, next-step thumbnail, notes and
elapsed timer in this same window. Shift+Enter / Play from here starts at the
selected build row's preceding boundary (or the cursor retained on exit).

**Smart matching.** One-to-one matches prefer equal ids, then equal nonempty
names in tree order. Deck ids remain globally unique, so across persisted
slides names are the practical matching key. Both rendered trees are measured
before FLIP transform/opacity animations. A matched node whose
appearance is unchanged (same text, fill, color, font) simply moves: shapes and
images translate, rotate and scale about their centers, text translates at its
native size (never an anisotropic scale) and the old copy is hidden. If the
appearance changed (fill, font size or color, or text content) the new node
moves in from the old position while the old one fades out, a crossfade, so no
colors or glyphs are interpolated. The incoming slide background fades in as its
own layer. Matched frames crossfade in place so child transforms are not
compounded. Unmatched nodes fade. Motion uses only transform/opacity/clip-path/filter;
`prefers-reduced-motion` replaces every effect and transition with a fade, and
halves durations, delays and stagger; emphasis becomes a brief opacity dip. Core PNGs and the editor always show all
nodes in their authored final state, ignoring even build-out visibility; the
model's `read` adds a `transition: push left 400ms ease_out` line and a compact
numbered build order per slide, for example
`builds: 1 click fade b2 · 2 with-prev rise b3 · 3 click emphasis pulse b2`. The field
reference and new operation names appear only in the once-per-session schema,
not the unchanged tool description or prompt golden.

### Migration from version 1

Decks load as version 2 whatever their file says, and the first save writes
version 2. The migration is idempotent and keeps ids:

- grid units become px (× 60); each block becomes an absolute, fixed-size child
  of a `mode: none` slide, in paint order;
- text size steps map to px (`s` 26, `m` 42, `l` 58, `xl` 81, `xxl` 119) and
  `xl`/`xxl` keep their tighter 1.08 line height; only values that differ from
  the default style are written; `style.align` and `style.color` become
  `font.align` and `font.color`;
- text and tables use the `body` style, code the `code` style; a shape's
  `stroke` becomes `{color, width: 2.4}`; the slide's template name is dropped;
- comment sidecars written for the grid keep working: their anchors (grid cells)
  become the cell's pixel origin plus 30 on load.

### Layout

`deck::layout(deck, slide)` returns every node's rectangle in slide px (tree
order). It maps onto flexbox, which the editor's CSS follows too:

| Deck | Flexbox |
|---|---|
| `row` / `column`, `gap`, `padding` | `display: flex`, `flex-direction`, `gap`, `padding` |
| `wrap` | `flex-wrap: wrap`, `align-content: flex-start`, `gap_cross` is the line gap |
| `main` / `cross` | `justify-content` / `align-items`; `stretch` also stretches `hug` children |
| size `fixed(n)` | `width`/`height: n`; on the main axis `flex: 0 0 auto` |
| size `hug` | `auto`; on the main axis `flex: 0 1 auto` (text wraps rather than overflow) |
| size `fill` | main axis `flex: 1 1 0`; cross axis `align-self: stretch` |
| absolute position | `position: absolute; left; top` from the parent's box edge, ignoring padding; `fill` pins the far edge to 0 |
| `stroke` | drawn inside the box, never takes layout space |
| `hidden` | `display: none` |

Text is measured with the renderer's own glyph metrics (`fontdb`, including
imported deck fonts): max-content is the widest unwrapped line, min-content the
longest word; the last paragraph or heading has no trailing margin. Images,
charts and shapes have a natural size (images follow their file, charts 640 ×
360, shapes 160 × 160) and no automatic minimum, so `fill` can shrink them.
A `hug` frame with `mode: none` collapses (absolute children give it no
content): give it a fixed size or use auto-layout.

### Operations (edits and suggestions use the same language)

`add_slide{after, slide}`, `delete_slide{id}`, `move_slide{id, to}`,
`set_notes{slide, notes}`, `update_slide{id, fields}`,
`insert_node{parent, index?, node}`, `update_node{id, fields, base_hash}`,
`delete_node{id}`, `move_node{id, parent, index?}`, `wrap{ids, layout, id?}`,
`unwrap{id}`, `set_layout{target, layout}`, `set_text_style{name, font}`,
`set_theme{theme}`. `parent` and `target` are a slide or frame id.

`update_node` and `update_slide` take a JSON merge patch (objects merge, `null`
removes a key); `id`, `kind` and `children` cannot be patched. `set_layout`
converts children like Figma: none → row/column makes them auto (ordered by
position), the reverse makes them absolute where they are, and a hugging frame
keeps its size. `wrap` puts siblings in a new frame (hug for auto-layout, the
exact bounding box for `none`); `unwrap` dissolves one; both keep the picture.

`deck::apply(&Deck, &[Op]) -> Result<Deck>` is atomic. `base_hash`, the hash of
the node as the caller last saw it, makes a stale update fail cleanly; within
one transaction a node may be updated more than once. `suggest::prepare` fills
`base_hash` and the ids of inserted nodes. Undo and redo in studio are inverse
operations, and the file saved on disk is always a whole deck.

## The `deck` tool

One public tool with seven actions (the model sees it as `deck`):

| Action | Does | Read-only |
|---|---|---|
| `create {title, path?, slides?}` | New deck under `decks/` (slug from the title); optional slide objects validated like a file; returns path, revision and a contact sheet | no |
| `read {path}` | The node tree with resolved rectangles, sizing, text styles and overrides, text (truncated per node), revision, comment threads; the first `read`, `create` or `edit` of a session also returns a compact field reference | yes |
| `fonts {path, text?}` | The deck's font roles and files, and up to 30 installed families matching the filter (`text`) | yes |
| `view {path, slides?}` | Renders slides: the ones named, else the slide open in studio (when linked), else the whole deck as a contact sheet | yes |
| `edit {path, ops, revision?}` | Applies `deck::Op`s atomically; fills `base_hash` and insertion ids; returns the new revision, a line per op and images of the changed slides | no |
| `comment {path, slide, block?, text, severity, ops?}` | Pins a model comment (`must`, `should`, `consider`), optionally with suggested ops; with `thread` it replies in that thread instead | no |
| `resolve {path, thread}` | Resolves a thread | no |

- **Confinement.** Paths are relative to the space's files and must be
  `decks/…/name.deck.json` with only letters, digits and `- _ . /`; no `..`,
  hidden folders or symlinks. Everything it writes, including the render cache,
  stays under `decks/`.
- **Errors recover.** A bad operation writes nothing and the error names the
  operation (`ops[1].id: no block "x" on slide "s2"`). A stale `revision`
  refuses with the current revision and says to `read` again.
- **Images.** Results contain `![slide 3](decks/.renders/<deck>/<slide>-<rev8>.png)`;
  the chat loop reads those files and gives them to vision models. A render is
  named by deck revision and an older revision of the same slide is removed.
- **Approval.** `read` and `view` are read-only (allowed in `batch`); the rest
  write space files like the `scripts` and `app` tools, with no approval prompt.
- The tool description carries the **commenting principles**: anchored to a
  slide and block; one issue in at most two sentences, with why it matters;
  actionable (attach ops when a fix exists); few and prioritized (at most
  eight per review) with a severity; respectful of the author's intent and
  facts; answering the thread it is in.

## The renderer

`deck::render` places every node where `deck::layout` puts it, draws the slide
as SVG (frames with fill, stroke, radius and clip; numeric typography with
weight, italics, line height and letter spacing; imported fonts) and rasterizes
it with `resvg`, measuring text with the font database `resvg` renders with, so
line breaks come from real glyph widths, not an estimate. Layout numbers (heading scale, paragraph and
heading spacing, list indent, table and code padding, chart box) live in one
table, `render::LAYOUT`, and `studio.css` and `render.js` use the
same numbers; a studio test fails if either drifts. Text that does not fit its
block box stays visible past the box, in both renderers, and is clipped only at
the slide edge, so the model sees overflow exactly as the author does.

Where the SVG differs from the HTML, deliberately: font stacks name platform
fonts (`system-ui` and `ui-serif` are CSS-only); table borders scale
with the slide instead of staying 1 CSS pixel; the inline-code tint is the
light one; and a baseline can sit a pixel off a browser's rounding of the same
font metrics. Remote images are drawn only when the vetted fetcher already
cached them, and never fetched here.

## The link

A newline-delimited JSON protocol, version 1 (`deck::link`), one envelope per
line: `{"version":1,"type":"…",…}`. Messages over 64 KB are rejected.

| Direction | Message | Meaning |
|---|---|---|
| studio → nexus | `Hello{deck_path}` | a deck is open |
| studio → nexus | `Comment{deck_path, thread_id, slide, block?, point, text}` | Send to nexus on a new pin |
| studio → nexus | `Reply{deck_path, thread_id, text}` | a reply in a thread, Send to nexus |
| studio → nexus | `Focus{deck_path, slide, blocks}` | what is on screen and selected (debounced 300 ms) |
| nexus → studio | `Open{deck_path}` | switch to a deck (the same deck is a no-op, undo history is kept) |
| nexus → studio | `Focus{deck_path, slide}` | the model changed this slide; jump to it |
| nexus → studio | `Theme{colors}` | terminal semantic roles as plain hex; chrome only |
| nexus → studio | `Bye` | the conversation ended |

**Transport today:** the TUI launches `nexus-studio --attach <deck>` as a child
with piped stdio. Studio reads lines on stdin and writes them on stdout (its own
logs go to stderr); EOF on stdin closes the window, EOF on stdout tells nexus the
window closed. The terminal side is view state in `crates/tui/src/studio.rs`:
no new `AppEvent`, and core only shares a small `deck_link` context with the
tool (what studio shows, which decks the model changed). The web app will speak
the same lines through `nexus host`, which is why the types live in core.

**In the conversation:**

- `Comment` and `Reply` become a user message:
  `Comment on decks/a.deck.json · slide 2 (b4: Make room…): text`, the thread
  id, and the slide as `![slide 2](decks/.renders/…png)`, the same Markdown
  image form a pasted image uses. The image is a durable snapshot, not a cache
  entry, because the message is replayed every turn.
- It is sent when nothing is in the way. A reply is streaming: the comment waits.
  The composer holds a draft: the draft is never overwritten, a status line says
  the comment is waiting, and it sends once the draft is sent or cleared.
- The first message after studio's `Focus` changes carries one extra line,
  `studio: viewing slide 3, selected b4`.
- When the `deck` tool creates or edits a deck, the terminal sends `Open` (if
  the deck differs) and `Focus` for the first changed slide. With no window
  attached, the tool card says `open in studio · /studio`.
- Opened, the tool card shows the rendered slides inline, with the op summaries.

## Phases

- **S1, app skeleton and canvas (done).** Deck schema, validation, `apply`,
  revisions; the `nexus-studio` crate with the embedded UI; slide strip;
  canvas editing; properties panel, speaker notes, undo/redo, presenting; the
  `nexus studio [path]` launcher.
- **S2, studio as the conversation's canvas (done).** The `deck` tool and the
  renderer; the link and `/studio`; autosave and external-change reload;
  comments with suggested edits; the deck card in the TUI. This replaced the
  first S2 (a studio chat pane and `POST /v1/studio/suggest` on the host),
  which is removed.
- **S3, import and export.** `.pptx` import and export, and PDF. Import and
  export run through a sandboxed Python venv with `python-pptx`, the way skills
  run. PDF comes from the webview's print path. Publish `nexus-studio` to
  crates.io and add it to the release workflow.
- **S4, web app.** The web client's deck view speaks the link through the host
  and renders the same decks; the web editors plan in `docs/web-editors.md`.
- **S5, mini-apps and remote.** Open `app` tool web apps in sandboxed studio
  views, each in its own origin with no IPC. Docs editing comes next, built on
  the same operation and comment model. Remote hosts use the device-key pairing
  in `docs/web-editors.md` §1.

## Editor status

The studio UI renders version-2 decks with real flexbox (`ui/render.js` follows
the layout table above) and edits them through v2 operations. Nested selection,
in-place text editing, duplication, deletion, comments, image/shape/text
insertion, zoom/pan and presenting remain available. D1 adds:

- **Wrap / unwrap:** Shift+A wraps siblings in a hugging auto-layout frame,
  guessing row versus column from their positions and using the median gap.
  Alt+Shift+A unwraps frames. Both actions are also in the canvas context menu
  and auto-layout panel.
- **Design panel:** a 256 px sidebar with collapsible sections, 28 px controls
  and paired, in-field numeric labels. Alignment icons sit above X/Y and W/H;
  sizing modes are inline Fix/Hug/Fill menus, Ignore auto layout is an icon
  beside X/Y, and min/max is behind “…” in the Layout / position header.
  Appearance pairs opacity/radius and exposes swatches with role-or-hex inputs.
  Chrome uses only terminal palette tokens, with labels/numbers at least 11 px.
- **Auto-layout:** none/down/right/wrap icon segments, a fixed gap with an auto
  (space-between) toggle, cross gap for wrapping, horizontal/vertical padding
  with a four-side toggle, and a direction-aware 3×3 alignment grid. Collapsing
  padding keeps differing sides intact and displays Mixed. Clip and stretch are
  icon toggles; the header offers wrap (+, Shift+A) and unwrap (−, Alt+Shift+A).
  A flowing text selection also exposes its parent's layout, so layout and
  typography fit together at 1440×900 without scrolling.
- **Canvas ordering:** drag flowing children to reorder, with a translucent
  ghost and a 2-screen-pixel accent insertion line. Drop over another
  auto-layout frame to reparent. Ctrl/Cmd+drag escapes into absolute slide
  positioning, or into a free-layout frame under the pointer. A gesture sends
  one atomic `Apply` transaction, so one undo restores it.
- **Free positioning:** drag and resize snap within 6 screen px to sibling and
  slide edges/centers. Error-role guides are 1 screen px; nearest-neighbour
  distances and equal-spacing brackets show slide-pixel gaps once per measured
  segment. Snap lines span only the moving and target nodes, with a small
  overshoot; crowded distance pills are offset so they do not overlap. Shift constrains
  dragging; Alt with a selection measures to the hovered node or parent edges.
  Arrows nudge 1 px, Shift 10 px. Resizing a hug/fill axis fixes that axis;
  Shift preserves aspect ratio and Alt preserves the center. Escape cancels
  an in-progress drag or scrub.
- **Align / distribute:** the panel offers six alignments, horizontal/vertical
  spacing distribution (3+ nodes) and tidy up. Alt+A/H/D aligns left/center/right,
  Alt+W/V/S top/center/bottom. A single free node aligns to its parent; multiple
  free nodes align to their bounding box. Flowing selections change their
  parent's main/cross alignment instead of acquiring absolute positions.
- **Numeric fields:** X/Y/W/H, rotation, opacity, radius, gaps, padding and type
  metrics accept arithmetic (`24*2`, `+8`, parentheses). Drag a label to scrub
  with a live local preview; Shift multiplies by 10 and Alt by 0.1. Up/Down
  steps values, Shift by 10. Multi-selection differences display “Mixed”. A
  scrub commits once on release, not once per pointer move.
- **Typography:** searchable, keyboard-navigable family picker grouped into
  In this deck / Installed, with visible rows previewed in their own faces.
  The picker opens below the family field inside the panel, pushing subsequent
  fields down rather than covering their labels. Weight/size and line-height/
  letter-spacing share rows; size is scrubbable with a separate preset chevron.
  Alignment, vertical alignment, case and italic use icon controls. Text style
  is a compact header row, with detach/update/create in its “…” menu.
  Size presets span 12–200 px; weights come from fontdb and registered deck
  files, with variable weight controls when available. Italics, line-height
  auto/multiplier, letter spacing (%), alignment, vertical alignment, case,
  theme colors and hex are editable. Styles can be applied, detached into
  resolved overrides, updated from selection or created. Detach clears the named
  style; v2 restores its implicit body/code fallback, but complete overrides
  keep the text independent and the picker displays Detached.
- **Import font…:** the webview's file chooser accepts TTF/OTF/WOFF2. The backend
  bounds imports to the core's 16 MB cap, sanitizes and hashes filenames,
  refuses other types and escaping/symlink destinations, copies into
  `assets/fonts/` and registers `theme.font_files` as one undo step. Fontdb
  supplies family/weight/italic from readable faces. WOFF2 is webview-only:
  its filename supplies the family when unreadable, with regular weight and
  the deck body font as PNG fallback.
- **Layers:** a left-column tree beneath the slide rail; drag the rail's lower
  corner to resize the split and give Layers more room. Double-click names to
  rename; hide/show and lock/unlock buttons;
  expand/collapse frames; drag above/below to reorder or into a frame's middle
  to reparent. SVG kind icons and selection track the canvas. Locked nodes
  (and descendants of locked frames) cannot be selected/edited on canvas,
  but remain selectable in Layers.

New IPC: `ListFonts {}` → `Fonts {families: [{family, weights, italic,
variable}]}` uses the renderer's cached system font database plus deck faces.
`ImportFont {path}` supports explicit local paths like image import;
`ImportFontData {name, data}` carries hex-encoded chooser bytes without exposing
local paths to JavaScript, and returns the normal `Deck` event. Only font-data
commands may exceed the normal deck-command limit; the decoded cap is 16 MB.

It cannot yet export/import PowerPoint or PDF (S3), render WOFF2 in core PNGs,
or play animations anywhere but present mode. Smart text/font/color changes crossfade rather than interpolating glyphs or
color channels, to keep playback compositor-only. Font previews use the
webview's installed faces, so platform font availability still matters.

## Tests

- **Core:** deck round-trips, validation paths, version-1 migration (idempotent,
  every kind, the classic fixture), every operation, atomic failure and stale
  `base_hash`; the layout engine (rows, columns, wrap, fill/hug, absolute,
  text measure) and **browser parity**: `ui-dev/fixtures/parity-*.deck.json`
  laid out by taffy against `rects-*.json` from Chromium
  (`node crates/studio/ui-dev/rects.mjs`; frames within 2 px, text-sized boxes
  within 6 px, text decks skipped where the font is not installed); the renderer (size, ink in every block,
  wrapping that follows glyph widths, overflow, contact sheets, the cache);
  every `deck` action in a temp space (confinement, stale revisions, located
  errors, image references in results); the link protocol's encode/decode.
- **Terminal:** a studio comment becomes a queued message with an image
  reference; a typed draft is never overwritten; streaming and a missing model
  keep it queued; focus is reported once per change; the deck card.
- **Studio:** autosave, an external edit as one undo step with the changed
  blocks flagged, the file winning a conflict, comments synced through the
  shared file, suggested edits (preview, apply as one undo step, dismissed and
  outdated), link-gated sending, and a check that the HTML renderer uses the
  shared layout numbers. No windows open in tests.
- **Screenshots:** `node crates/studio/ui-dev/shot.mjs` (Playwright from
  `web/node_modules`) drives the UI against a mock backend and writes
  1440×900 light and dark shots to `crates/studio/ui-dev/shots/`.
  `NEXUS_RENDER_OUT=<dir> cargo test write_fixture_renders` (core) and
  `write_core_renders_of_the_dev_deck` (studio) write core-renderer PNGs to
  compare with them.
- **D1 comment/identity harness:** `node crates/studio/ui-dev/d1-shot.mjs`
  uses a dark deck at 1440×900 in the default nexus palette and the checked-in
  light Alacritty fixture. It checks safe Markdown, operation-derived edit
  titles, preview/apply/undo, pin non-overlap and constant 24 px sizes at
  0.5× / 1× / 2× native zoom, pointer-anchored zoom, Space+drag and two-axis
  scroll panning, fit/100% shortcuts, and terminal colors winning over opposite
  OS preferences.
  Outputs: `ui-dev/shots/d1-{comments,applied,layers}-{default,light}.png`
  and `d1-pin-geometry.json` (under `crates/studio/`). The original screenshot
  harness and existing tests are unchanged. New Rust tests cover portable
  colors, fixture palette equivalence, theme link round trips, and a theme
  update leaving the saved deck and undo history untouched.
- **D1 editor harness:** `node crates/studio/ui-dev/d1b-shot.mjs` checks wrapping,
  layout modes/grid/gaps/padding/min-max/clip, snapping within 1 px, screen-space
  guides/distances/equal gaps, reorder/reparent/absolute escape, align/distribute/
  tidy, arithmetic/steps/scrubbing/mixed values, font search/keyboard/weights/
  styles/native chooser-byte transport, layers rename/hide/lock/collapse/drop,
  resize modifiers and cancellation. It also checks paired padding preserving
  individual values, inline font-picker bounds/non-occlusion, common-section
  fit, unique gap pills, local guide spans and crowded-label non-overlap.
  It asserts that drag and scrub previews send no edits, and release sends one
  `Apply` and adds one undo step. Outputs are 1440×900 in default nexus and the
  light Alacritty fixture: `ui-dev/shots/d1b-{auto-layout,guides,reorder,fonts,
  layers,align}-{default,light}.png` (gitignored). `backend/font_tests.rs` covers
  cached font listing, imported metadata/theme/undo, chooser-byte transport,
  type/data/size/path rejection, symlink confinement and the serving size cap.
- **D2 animation harness:** `node crates/studio/ui-dev/d2-shot.mjs` checks
  click/with/after groups, leading automatic builds, reverse/jump boundaries,
  id/name smart matching, reduced-motion half-duration fades, one undo per
  animation edit/scrub/reorder, no-write previews, canvas badges, Play from here,
  presenter notes and next-step thumbnails. Outputs at 1440×900 in both terminal
  palettes: `ui-dev/shots/d2-{animate,badges,push,smart,presenter}-{default,light}.png`.
  Push and smart frames are captured halfway through paused WAAPI animations.
  New Rust animation tests cover typed payload defaults/round trips, operation
  validation/atomicity/clearing, build-order repair, merge-patch preservation,
  read summaries, schema-once references and backend Apply/undo/redo/resources.
- **Hermetic:** no network, and no windows opened in Rust tests. Browser harnesses
  use loopback-only static fixtures and mock IPC.
