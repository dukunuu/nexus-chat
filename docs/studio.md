# Design: nexus studio — a separate desktop app for decks (then docs and mini-apps)

Status: design, 2026-10-21. Supersedes the editor half of
`docs/web-editors.md`; that document's device-key pairing stays the plan for
remote access (phase S5).

## Decision

The user chose **a built-in separate app** over a browser editor or a TUI
canvas: "so that we don't get hindered, it connects to the host for edit
suggestions. and support for miniapp later on". Decks come first.

So: `nexus studio` opens **nexus studio**, a native desktop window that
ships with nexus. Studio edits files directly on this machine and asks
**`nexus host`** for model suggestions. It does not depend on the TUI, the
`web/` client, or the host's remote-control story, so neither can hold it
back. Later it also runs mini-apps (the `app` tool's web apps) in sandboxed
views.

## Shape

| Piece | Where | Notes |
|---|---|---|
| Deck schema, validation, operations, revision hashes | `crates/core/src/deck.rs` | pure data; shared by studio, host, and a future chat `deck` tool; no UI deps |
| Desktop app | `crates/studio` (package `nexus-studio`, binary `nexus-studio`, `publish = false` until S3) | `tao` window + `wry` webview; Rust backend owns files and the host connection |
| Studio UI | `crates/studio/ui/` (HTML/CSS/vanilla JS modules, embedded with `include_str!`) | no Node build step, so `cargo install` works offline from the repo |
| Launcher | `nexus studio [path]` in `crates/tui/src/cli.rs` | runs `nexus-studio` from PATH or beside the `nexus` binary; says how to install it when missing |
| Suggestions | `nexus host`: `POST /api/studio/suggest` | studio talks to a local host on loopback with the existing host token; remote hosts need S5 pairing |

Why a webview rather than drawing natively: slides are layout plus type,
and HTML/CSS does both well. The same renderer later produces thumbnails
and PDF. `wry` uses the system webview (WKWebView on macOS, WebView2 on
Windows, WebKitGTK on Linux), so nothing heavy is bundled. Linux CI and
installs need `libwebkit2gtk-4.1-dev`.

### Boundary between UI and backend

The webview UI is untrusted presentation. It reaches the backend only
through one IPC channel (`window.ipc.postMessage` → a typed JSON command
enum in Rust), and the backend answers with events. The UI never gets file
paths outside the opened deck's folder, never sees the host token, and has
no network access: a strict CSP (`default-src 'self'; img-src 'self' data:
nexus:`) plus a custom `nexus://` protocol that serves only the embedded UI
and the deck's own assets. Remote images in decks go through
`nexus_core::remote_image` (the same vetted fetcher as the TUI) and reach
the page as `nexus://asset/<hash>`.

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

## Suggestions (S2)

1. In studio you select slides or blocks, or nothing for the whole deck,
   and type an instruction ("tighten slide 3", "make this two columns").
   You can also click "Suggest" for an open-ended review.
2. Studio sends `{deck, revision, selection, instruction}` to the host.
3. The host runs one model call with a compact deck-editing prompt and the
   session's model. The model must reply with operations plus a one-line
   `why` per group. The host validates them with `deck::apply` on a copy
   and makes one automatic repair attempt with located errors, the same
   loop as nexus-ui repair. It returns `{ops, why, preview_revision}`.
4. Studio shows each suggestion as a ghost overlay with a before/after
   toggle, and you accept or reject each one, or all at once. Accepting
   applies the operations locally. A suggestion whose `base_hash` no
   longer matches shows as outdated.

The host never writes the deck: it only proposes changes. Studio keeps a
history of the suggestions it received in `<deck>.suggestions.json`.

## Phases

- **S1, app skeleton and canvas.**
  - `deck.rs`: the schema, validation, `apply`, revisions and tests.
  - The `nexus-studio` crate: a window with the embedded UI, opening and
    saving decks, and a new deck from a layout set.
  - A slide strip with reorder and add/delete.
  - Canvas editing: select, drag and resize snapping to the grid, inline
    text editing, keyboard nudges.
  - A properties panel, speaker notes, undo/redo, and presenting full
    screen.
  - The `nexus studio [path]` launcher and CI WebKitGTK setup.
- **S2, host suggestions.** The `/api/studio/suggest` endpoint, the
  deck-editing prompt and the repair loop, plus a connection indicator in
  studio. Studio starts a loopback host when none is running.
- **S3, import and export.** `.pptx` import and export, and PDF.
  Import and export run through a sandboxed Python venv with
  `python-pptx`, the way skills run. PDF comes from the webview's print
  path. Images come from the deck's assets folder.
  Publish `nexus-studio` to crates.io and add it to the release workflow.
- **S4, decks from chat.** A model `deck` tool creates or edits
  `*.deck.json` in the space's files ("make a deck from this doc"), and
  the TUI card offers "open in studio".
- **S5, mini-apps and remote.**
  - Open `app` tool web apps in sandboxed studio views, each in its own
    origin with no IPC.
  - Docs editing comes next, built on the same operation and suggestion
    model.
  - Remote hosts use the device-key pairing in `docs/web-editors.md` §1.

## Tests

- **Core:** `deck` round-trips, validation paths, clamping, every
  operation, atomic failure, stale `base_hash`, and the revision hash being
  stable across key order.
- **Studio:** the IPC command parser rejects paths outside the deck folder
  and unknown commands, and the `nexus://` protocol serves only embedded
  files and deck assets. The UI's pure JS logic (grid snapping, the
  operation builder) has tests run by a small Rust harness if needed;
  no browser runs in CI.
- **Hermetic:** no network, and no windows opened in tests (window
  creation stays behind `main`).
