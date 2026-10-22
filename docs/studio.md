# Design: nexus studio — the canvas of the conversation (decks first, then docs and mini-apps)

Status: terminal-theme identity and revised comments are implemented; deck data
and editing remain version 1. D1's version-2 node tree, auto-layout, numeric
typography, imported fonts, and taffy/browser parity are not implemented yet.
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
| Deck schema, validation, operations, revision hashes | `crates/core/src/deck.rs` | pure data; shared by studio, the `deck` tool and the renderer; no UI deps |
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
`why` is just "Suggested edit". Existing v1 size steps are displayed using the
renderer's native 1600 px canvas sizes (21.6 / 35.2 / 48 / 67.2 / 99.2 px),
not a claim of numeric typography support. Applied cards say
"Applied · undo with Cmd+Z"; the backend still saves an application as one undo
step.

Canvas navigation supports Ctrl/Cmd+scroll (and native webview pinch) to zoom
around the pointer, Space+drag or two-axis scroll to pan, Shift+1 to fit and
reset pan, and Shift+0 for native 100%. Numeric zoom remains fixed while the
window resizes; fit follows the available canvas. Until v2, native 100% means
the existing renderer's 1600 px slide width, not the proposed 1920 px frame.

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

## Deck format (`*.deck.json`, version 1)

```json
{
  "version": 1,
  "title": "Q3 review",
  "theme": {
    "colors": {"bg": "#0f1115", "fg": "#e8e6e3", "accent": "#7aa2f7", "muted": "#8b8f98"},
    "fonts": {"heading": "Inter", "body": "Inter", "mono": "JetBrains Mono"}
  },
  "slides": [
    {
      "id": "s1",
      "layout": "title",
      "blocks": [
        {"id": "b1", "kind": "text", "x": 2, "y": 6, "w": 28, "h": 4,
         "text": "# Q3 review\nRevenue up **18%**", "style": {"align": "left", "size": "xl"}},
        {"id": "b2", "kind": "image", "x": 20, "y": 2, "w": 10, "h": 8, "src": "assets/chart.png", "fit": "cover"}
      ],
      "notes": "Open with the headline number."
    }
  ]
}
```

- **Canvas**: 16:9 as a 32×18 grid. Block positions and sizes are integer
  grid units, clamped inside the slide.
- **Blocks**: `text` holds a Markdown subset (headings, bold, italic,
  bullets, numbered lists, inline code, links). `image` has a `src` that
  is either relative to the deck's `assets/` folder or https through the
  vetted fetcher. The others are `shape` (rect, ellipse, line, arrow, with
  fill and stroke as theme roles or hex), `table` (rows of cells), `code`
  (language and source) and `chart` (bar, line or pie, with labels and
  series).
- **Layouts**: title, section, content, two-column, image-text, quote,
  blank. A layout is a starting arrangement only; blocks own their
  positions.
- **Theme**: colors and fonts are tokens. Style values name a role
  (`accent`, `muted`, …) or give a hex color.
- **Ids** are stable strings unique within the deck, and a **revision** is
  the sha256 of the canonical JSON. Validation is forgiving where the
  intent is clear: it clamps positions and fills in missing ids, the same
  approach as `interactive::check`. Every error it reports names a JSON
  path.

### Operations (edits and suggestions use the same language)

`add_slide{after, slide}`, `delete_slide{id}`, `move_slide{id, to}`,
`set_layout{slide, layout}`, `set_notes{slide, notes}`,
`insert_block{slide, block}`, `update_block{slide, id, fields, base_hash}`,
`delete_block{slide, id}`, `restyle{target, style}`, `set_theme{theme}`.

`deck::apply(&Deck, &[Op]) -> Result<Deck>` is atomic. `base_hash`, the
hash of the block as it was when the suggestion was made, makes a stale
suggestion fail cleanly instead of overwriting newer edits. Undo and redo
in studio are inverse operations, and the file saved on disk is always a
whole deck.

## The `deck` tool

One public tool with six actions (the model sees it as `deck`):

| Action | Does | Read-only |
|---|---|---|
| `create {title, path?, slides?}` | New deck under `decks/` (slug from the title); optional slide objects validated like a file; returns path, revision and a contact sheet | no |
| `read {path}` | Slide ids and layouts, block ids, kinds, rectangles and styles, text (truncated per block), revision, comment threads | yes |
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

`deck::render` draws a slide as SVG and rasterizes it with `resvg`, measuring
text with the font database `resvg` renders with, so line breaks come from real
glyph widths, not an estimate. Layout numbers (font sizes, line heights,
paragraph and heading spacing, list indent, table and code padding, chart box)
live in one table, `render::LAYOUT`, and `studio.css` and `render.js` use the
same numbers; a studio test fails if either drifts. Text that does not fit its
block box stays visible past the box, in both renderers, and is clipped only at
the slide edge, so the model sees overflow exactly as the author does.

Where the SVG differs from the HTML, deliberately: font stacks name platform
fonts (`system-ui` and `ui-serif` are CSS-only); shape and table borders scale
with the slide instead of staying 1–2 CSS pixels; the inline-code tint is the
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

## Tests

- **Core:** deck round-trips, validation paths, every operation, atomic
  failure and stale `base_hash`; the renderer (size, ink in every block,
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
- **Hermetic:** no network, and no windows opened in tests.
