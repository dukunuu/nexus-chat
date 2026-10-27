# Design: docs in nexus studio, one design system, and templates

Status: design, 2026-10-27. Builds on `docs/studio.md` (decks, the `deck`
tool, the link, exports, import). Phases are at the end.

The user asked for:
- a Claude Docs–like app on the studio canvas, for drafting documents;
- slide generation whose design is consistent;
- the ability to use templates once they are uploaded.

Decisions taken with the user:

- **Docs flow.** A doc is one continuous column at reading width, like Claude
  Docs or Notion. Pages exist only in exports and in a page-preview toggle.
- **One design system for docs and decks, plus slide layouts.** A space's
  design (colors, fonts, type scale, page setup, slide layouts) is shared by
  its docs and decks. Generated slides are built from named layouts, so a deck
  follows one system from slide to slide.
- **Real PDFs.** Docs and decks export vector PDFs with real, selectable,
  embedded-font text through `krilla` + `krilla-svg` 0.8. They are typst's PDF
  stack and are built on the `resvg`/`usvg` 0.47 we already use, so there is
  one SVG stack. This replaces the deck PDF's raster pages and hidden text
  layer.

Unchanged from `docs/studio.md`:
- the conversation stays in the TUI (studio has no chat);
- the file is the single source of truth;
- model edits land directly and are undoable;
- reviews are anchored comments carrying optional suggested edits;
- the UI is untrusted presentation behind one typed IPC channel.

## Shape

| Piece | Where | Notes |
|---|---|---|
| Design system | `crates/core/src/design/` | `*.design.json`: tokens, text styles for slides (px) and docs (pt), page setup, slide layouts; validation, defaults, lint |
| Doc schema, validation, ops, revisions | `crates/core/src/doc/` | pure data, like `deck/`; shares `deck::history` and `deck::feedback` (comments) by generalizing them over a document kind |
| Doc layout and pages | `crates/core/src/doc/layout.rs`, `doc/render.rs` | flowing blocks measured with `deck::text`, paginated for preview, PNG view and PDF |
| Exports and imports | `crates/core/src/doc/{export,import}/`, `deck/export/pdf.rs` | DOCX, PDF, Markdown, HTML out; DOCX/DOTX and Markdown in; deck PDF becomes vector |
| Templates | `crates/core/src/design/template/` | `.pptx/.potx/.docx/.dotx` to a design, and export into the template's own package |
| The `canvas` tool | `crates/core/src/tools/canvas.rs` (today `deck.rs`) | one public tool for decks and docs (it replaces `deck`; `deck` stays a dispatch alias) |
| Studio doc editor | `crates/studio/ui/doc/`, `crates/studio/src/backend/doc.rs` | block editor, comments with tracked-change suggestions, outline, page preview, export |
| Link | `crates/core/src/deck/link.rs` → generalized | `path` plus a `kind`, anchors for text ranges |

## The design system (`<space files>/design/<slug>.design.json`)

One file per design. The space's **default design** is named in
`design/default` (a one-line file holding a slug), and is `nexus` when it's
missing. Designs come from the built-in default, from an uploaded template
(see *Templates*), or from the model or the user editing one.

```json
{
  "version": 1,
  "name": "Acme",
  "source": {"template": "templates/acme-brand.potx", "imported_at": "2026-10-27T09:00:00Z"},
  "colors": {"bg": "#ffffff", "fg": "#1d1d1f", "accent": "#0a6cff", "muted": "#6e6e73",
             "accent2": "#ff7a00", "surface": "#f5f5f7", "border": "#d2d2d7"},
  "fonts": {"heading": "Acme Sans", "body": "Inter", "mono": "JetBrains Mono"},
  "font_files": [{"family": "Acme Sans", "file": "assets/fonts/acme-sans-bold.ttf", "weight": 700}],
  "scale": {"base_pt": 11, "ratio": 1.25},
  "slide": {
    "size": {"w": 1920, "h": 1080},
    "text_styles": {"title": {"family": "heading", "size": 120, "weight": 700, "line_height": 1.08}},
    "spacing": {"margin": 120, "gap": 32},
    "layouts": {"title": {"…": "a slide frame tree with slots"}}
  },
  "doc": {
    "text_styles": {"title": {"family": "heading", "size": 28, "weight": 700},
                    "h1": {"…": "pt"}, "h2": {}, "h3": {}, "body": {}, "quote": {}, "caption": {}, "code": {}},
    "page": {"size": "a4", "margins_pt": [72, 72, 72, 72], "header": "", "footer": "{page}"},
    "measure_ch": 72
  }
}
```

- **Tokens** are shared: color roles, font roles, font files and the type
  `scale`. Slide sizes are in px (1920-wide frames); doc sizes are in pt. When
  a design gives no explicit text styles, both sets are derived from `scale`,
  so a heading in a report and on a slide are the same family, weight and
  color at the medium's own size.
- **Color roles** extend the deck's `bg fg accent muted` with `accent2`–`accent6`,
  `surface` and `border`. Templates fill as many as they define.
- **Slide layouts** are named slide frame trees (the deck v2 node tree) whose
  slot nodes carry `"slot": "<name>"`. The built-in set:
  - `title`, `section`, `content`, `two_column`, `image_left`, `image_right`;
  - `quote`, `big_number`, `comparison`, `agenda`, `closing`.

  A template's slide layouts become layouts with its placeholder positions,
  fonts and background art.
- **Validation** mirrors `Deck::check`: it fills missing tokens and styles from
  the built-in design, repairs ids in layouts, names JSON paths in errors, and
  sets limits (40 layouts, 200 nodes per layout).

### How decks and docs use it

- A deck's or doc's `theme` stays **a full, self-contained copy**, so files
  render and export without the design. A `design: "<slug>"` field records
  which design it follows, plus the design revision it copied.
- `create` copies the space's default design, or the one it names (`design`,
  or `template` with an uploaded template).
- `apply_design {design?}` is an operation on both kinds. It re-copies the
  design and re-maps nodes that follow roles and styles, and keeps explicit
  overrides. Studio shows "design updated · apply" when a deck or doc follows a
  design whose revision moved.
- **Slides from layouts.** `add_slide {layout, slots: {title: "…", body: "…",
  image: "assets/…"}, after?}` instantiates a layout. Slot text and images fill
  the slot nodes, and an empty slot is removed. The model is told to build
  slides from layouts and to use roles and styles. Freeform nodes remain legal.
- **Consistency lint.** Every `edit`/`create` result on a deck that follows a
  design ends with one `design:` line when something is off-system, for
  example `design: 3 nodes use hex colors outside the design (b4, b9, b12);
  2 font sizes are not text styles (b7 58px)`. These are hints and never
  errors. Studio shows the same count on the Design panel. This is what keeps
  generated slides consistent: the model sees drift right after it writes it.

## Doc format (`<space files>/docs/<slug>.doc.json`, version 1)

```json
{
  "version": 1,
  "title": "Q3 strategy",
  "design": "acme", "design_rev": "3f2a…",
  "theme": {"…": "a copy of the design's tokens and doc styles"},
  "page": {"size": "a4", "margins_pt": [72, 72, 72, 72], "header": "Acme · Confidential", "footer": "{page} / {pages}"},
  "blocks": [
    {"id": "b1", "kind": "title", "text": "Q3 strategy"},
    {"id": "b2", "kind": "paragraph", "text": "We grew **18%** while [churn](https://…) fell."},
    {"id": "b3", "kind": "heading", "level": 1, "text": "Goals"},
    {"id": "b4", "kind": "list", "ordered": false, "items": [
      {"text": "Ship the docs canvas", "children": [{"text": "Comments and suggestions"}]},
      {"text": "Templates", "checked": false}]},
    {"id": "b5", "kind": "quote", "text": "Make the report and the deck look like one company."},
    {"id": "b6", "kind": "callout", "tone": "info", "text": "Numbers are preliminary."},
    {"id": "b7", "kind": "code", "lang": "sql", "text": "select …"},
    {"id": "b8", "kind": "table", "header": true, "align": ["left", "right"], "rows": [["Region", "Revenue"], ["EU", "$4.1m"]]},
    {"id": "b9", "kind": "image", "src": "assets/chart.png", "alt": "Revenue by month", "caption": "Figure 1", "width": "full"},
    {"id": "b10", "kind": "chart", "chart": {"…": "the deck chart node's fields"}},
    {"id": "b11", "kind": "divider"},
    {"id": "b12", "kind": "page_break"},
    {"id": "b13", "kind": "toc"}
  ]
}
```

- **Blocks are a flat list**, so headings define the outline. Inline text is
  the same Markdown subset decks use (bold, italic, inline code, links), plus
  `~~strike~~`, footnote references `[^1]` with a `footnotes` map on the doc,
  and `{style: "caption"}` overrides on a block.
- **Kinds:**
  - text: `title`, `heading` (level 1–3), `paragraph`, `quote`, `callout`
    (info, warn, success, note), `code`;
  - lists: `list` (ordered, nested items up to 4 deep, optional `checked`);
  - media: `table`, `image` (inline, wide, full), `chart`;
  - structure: `divider`, `page_break`, `toc`.
- **Ids and revisions** work as in decks: stable ids, repaired when missing or
  duplicated, and a sha256 revision of the canonical JSON. Limits: 2,000 blocks,
  32 KB per block, 200 table rows by 20 columns.
- **Comments** live in `<doc>.comments.json` with the deck's thread model,
  generalized. An anchor is `{block, range?: {start, end}, quote}`, where the
  offsets count characters of the block's plain text and `quote` is the
  anchored text. After an edit, an anchor re-finds its `quote` in the block. If
  that fails, it degrades to the block, and a block that's gone leaves the
  thread on the doc.
- **History** reuses `deck::history` under `docs/.history/<stem>/`.

### Operations

Operations are used by the model, by studio and by suggestions:
- `insert_blocks {after?, blocks}` and `insert_markdown {after?, markdown}`,
  which parses Markdown into blocks (headings, paragraphs, lists, quotes,
  fenced code, tables, images, `---`);
- `update_block {id, patch}` and `delete_blocks {ids}`;
- `move_blocks {ids, after?}`;
- `replace_text {id, find, replace, occurrence?}`, an exact-match inline edit.
  This is what suggestions use, so studio can draw them as tracked changes;
- `set_page {patch}`, `set_title {title}` and `apply_design {design?}`.

They apply atomically and are checked in order. Errors name operations
(`ops[2].find: "Q4" not in b3`), with `fix:` hints, as in `deck::direct`.

## The `canvas` tool

The token ceiling (`CHAT_PROMPT_TOKEN_CEILING` = 3,960 for the system prompt
plus all tool definitions) is already at 3,957, so docs cannot be a second tool.
The `deck` tool becomes **`canvas`**, one tool for both kinds, chosen by path
(`decks/…​.deck.json`, `docs/…​.doc.json`) or by `create {kind}`.

- **Actions:** `create`, `read`, `view`, `edit`, `comment`, `resolve`,
  `history`, `export`, `fonts`, plus `design` (read or edit a design, list
  designs and templates, set the default).
- **Description.** It names both kinds and when to use them: a doc to draft or
  review writing, a deck to present. Docs start from Markdown (`create {kind:
  "doc", markdown}`). Slides come from layouts, and drift is reported. Field
  references stay out of the definition. The first result for each kind carries
  that kind's compact reference, as decks do today.
- **The budget holds.** Room comes from trimming the tool definitions; it is
  not raised. `media` (979 tokens) and the deck text are the obvious places.
  The ceiling test stays as is.
- **`read` on a doc** returns an outline, one line per block (`[b3] ## Goals`,
  `[b4] - Ship the docs canvas`), with long blocks truncated plus a `more`
  line. Given `blocks` or a heading id, it returns those blocks in full. It also
  returns the revision and the open threads with their anchors.
- **`view` on a doc** renders pages to PNG (preview pagination) for checking
  the look, and it's never needed for text. With `blocks`, it renders the pages
  holding those blocks.
- **`export`:** a doc gives `docx`, `pdf`, `md` or `html`; a deck gives `pptx`,
  `pdf`, `png`, `html` or `json`. A `template` exports into a template package
  (see *Templates*).
- **`deck` dispatches to `canvas`**, so replayed history and in-flight sessions
  keep working. Prompts and goldens move to `canvas`.

## Doc layout, pages and the renderer

- **Flow.** Studio lays the doc out with CSS. Core lays it out for pages: the
  blocks flow in the page's content box at the design's pt sizes, with text
  measured by `deck::text` (the same font database the renderer uses).
  Pagination follows a few rules:
  - headings keep with the next block;
  - a paragraph or list item doesn't leave a single line behind or carry one
    over (no widows or orphans);
  - table rows split between pages, with the header repeated;
  - images and code blocks move whole, unless they're taller than a page;
  - `page_break` forces a new page;
  - `toc` lists the headings with their page numbers.
- **One page renderer.** Each page is drawn as SVG and produces:
  - PNG through `resvg`, for `view` and studio's page preview;
  - PDF through `krilla-svg`, with fonts embedded and subset, real text,
    links, and an outline (bookmarks) from the headings;
  - deck slides, which go through the same SVG-to-PDF path, so deck PDFs
    become vector too (speaker notes stay out of exports, as today).
- **Parity.** Studio's preview toggle shows core-rendered pages, which are the
  PDF's own layout, so preview, `view` and the PDF never disagree. The flowing
  editor uses CSS that follows the same numbers, kept in one `doc::LAYOUT`
  table with a parity test, like `render::LAYOUT` for decks.

## Exports and imports (docs)

All are offline and deterministic. The default destination is
`<space files>/exports/<slug>.<ext>`, never overwriting.

- **DOCX** is hand-written OOXML (`zip`, no new dependency).
  - `styles.xml` comes from the design: Title, Heading 1–3, Normal, Quote,
    Caption, Code, List Paragraph, with colors and fonts.
  - Lists use `numbering.xml`, and checkboxes become ☐/☑ characters.
  - Tables are native tables with a header row that repeats.
  - Images are embedded, with their captions. Charts are 2x PNG pictures.
  - Page size, margins, header and footer use PAGE and NUMPAGES fields.
  - Links are hyperlinks and footnotes are real footnotes.
  - Open comment threads export as Word comments on their ranges.
- **PDF:** see above.
- **Markdown:** GitHub-flavored, with images relative to an `assets/` folder
  beside it.
- **HTML:** one file, with fonts and images as data URIs and a strict CSP.
- **DOCX/DOTX import** reads paragraphs by style:
  - headings come from outline level or built-in style name;
  - title, quote and code styles become their kinds;
  - runs become inline Markdown (bold, italic, strike, code, links);
  - `numPr` becomes lists, with levels;
  - tables, inline images and page breaks become their kinds;
  - footnotes and comments become threads.

  It is faithful first: anything it can't represent becomes a warning, never
  a failure.
- **Markdown import:** `create {kind: "doc", from: "notes.md"}` or a pasted
  `markdown`.

## Templates

A template is a `.pptx`, `.potx`, `.docx` or `.dotx` that a user uploads, by:
- dropping it on studio, or using the Design panel's "Add template";
- adding it to the space files (`/files`) and telling the model;
- or running `nexus design import <file>`.

Implemented in `design/template/`. Each upload is kept at
`<space files>/templates/<slug>.<ext>` and creates or refreshes
`design/<slug>.design.json` with `source.template`, `imported_at` and `sha256`.
`canvas {action:"design",op:"import",from:"brand.potx"}` imports a space file;
`nexus design import <file> [--name <name>]` accepts a local file and
`nexus design list` lists designs. Studio's chooser and file drop use bounded
chooser-byte IPC; explicit local paths use `ImportTemplate {path}`. Import
never applies a design without the author's separate **Use for this doc/deck**
action. The tool result includes colors, fonts, styles, layout slots and a
core-rendered layout contact sheet. The tool definition/goldens remain unchanged
(the import operation is accepted by dispatch but not yet advertised there).

- **From PowerPoint** (reusing `deck::import`'s theme and master reading):
  - theme colors become roles, and major/minor fonts become heading/body;
  - the master title, body and other text styles become slide text styles,
    and the doc styles are derived from the same scale;
  - the slide size;
  - each slide layout becomes a layout. Placeholders (title, body, picture,
    other) become slots at their positions with their fonts, and the master's
    and layout's background shapes and pictures become locked background
    nodes.
- **From Word:** theme colors and fonts (`theme1.xml`); the doc text styles
  (Normal, Title, Heading 1–3, Quote, Caption, a code style); page size,
  margins, header and footer text. The slide side is derived from the scale.
- **Exporting into the template**, so the result is genuinely the company's
  file and not a lookalike:
  - **PPTX:** the export reuses the template package's masters, layouts, theme
    and assets. Each slide built from a template layout references that layout
    and fills its placeholders. Freeform nodes are written as today.
  - **DOCX:** the export reuses the template's `styles.xml`, theme, numbering,
    settings, headers, footers and section properties. It writes only
    `document.xml`, media and relationships, and maps each block kind to the
    template's style id found by its built-in name (`Heading 1` → whatever id
    the template uses).
- A deck or doc following a template design exports into the matching Office
  package by default. `export {template: "none"}`, CLI `--template none`, and
  studio's **Plain file** write the original plain package. The studio Export
  menu defaults **Into template** on when the design has a source.
- **Extraction mapping and degradation:** PowerPoint title/ctrTitle become
  `title`, body/obj become `body`, repeated slots get numeric suffixes, pic
  becomes `image`, and subTitle becomes `subtitle`. Date/footer/slide-number
  placeholders are skipped with warnings. Layout names are slugged and deduped;
  `source_layout` and each slot's original placeholder type/idx survive
  instantiation. Master/layout artwork is locked and content-addressed under
  `design/assets/<slug>/`; it renders locally but is not duplicated into
  template slides. Master title, body levels 1–3 and other styles become
  title, body/subheading/lead and caption. Document styles derive from the same
  scale. The existing PPTX import approximations (unsupported pictures,
  effects, gradients and complex shapes) still apply and are reported.
- Word Normal, Title, Heading 1–3, Quote, Caption and Code/HTML Preformatted
  map to body/title/h1–h3/quote/caption/code; a monospace style can supply code.
  Run size, weight, italic, family and color follow style inheritance. Final
  section page size/margins and default header/footer text are imported;
  PAGE/NUMPAGES/TITLE fields become `{page}`/`{pages}`/`{title}`. Paragraph
  spacing cannot be stored in the current schema: a warning explains that
  preview uses `doc::LAYOUT`. Header/footer artwork, tables and unsupported
  fields are omitted from preview with warnings but preserved in Word. Other
  style names and unsupported style properties are reported and retained only
  in the template package.
- **Package reuse:** PowerPoint keeps the template's masters, layouts, themes,
  media and presentation properties, including notes/handout masters. Example
  slides and their notes are removed. Generated slides, media, slide
  relationships, presentation slide list and content types are written;
  template layouts receive real `p:ph` shapes. Freeform content uses a blank
  layout when available. Word keeps styles, themes, numbering, settings,
  font tables, headers/footers and the final `sectPr`; it replaces document
  content, content relationships, generated media, footnotes and comments.
  Style ids are looked up by case-insensitive built-in **name**, not by id;
  only used missing styles (and dependencies) are appended. Existing bullet
  and decimal numbering is reused; missing definitions are added with fresh
  ids. POTX/DOTX main content types become PPTX/DOCX. Both writers are
  deterministic. A brand imported from the other Office medium still supplies
  its shared design, but exports a plain package for that medium.
- **Safety:** templates are parsed with the bounded readers `deck::import`
  already uses (250 MB file, 20,000 entries, 24 MB XML/48 MB media parts,
  600 MB aggregate reads, bounded XML depth/elements). External relationships
  are never followed. `.potm`, `.pptm`, `.dotm`, `.docm`, VBA parts and disguised
  macro-enabled packages are refused and never copied.

## Studio: the doc editor

Opening a `.doc.json` puts studio in **doc mode**. The chrome is the same
terminal-themed shell, and the content follows the doc's design.

- **Layout:** an outline (headings) on the left, the page column in the
  middle at the design's `measure_ch`, and comments on the right. The top bar
  is slim: the title, undo and redo, the link pill, Design, Pages (preview) and
  Export. Formatting is a floating toolbar over selected text (style menu,
  bold, italic, strike, code, link, comment), and `/` inserts every block kind.
- **Editing:**
  - **one `contenteditable` surface** for the whole page (not one per block), so
    selections, arrow keys, spell checking and IME work across blocks; Enter,
    Backspace at a block's start, Tab, formatting, pasting and deleting across blocks
    are intercepted and become operations; Markdown shortcuts (`# `, `- `, `1. `,
    `[] `, `> `, ```` ``` ````, `---`, `**`, `_`, `` ` ``, `~~`);
  - `/` opens the insert menu;
  - Enter splits a block, Backspace at the start merges it into the previous
    one (or turns a heading or list item into a paragraph first), Tab and
    Shift+Tab nest list items;
  - a block handle drags blocks to move them or opens the block menu;
  - pasting HTML or Markdown becomes blocks; images pasted or dropped go to
    `docs/assets/`.

  Typing is saved through the same autosave as decks: operations batched
  per pause (about 700 ms), one undo step per batch, and external changes
  reloaded as one undo step with the changed blocks flashing.
- **Comments, the Claude Docs way:**
  - select text, then Comment (or Cmd+Alt+M); the thread is anchored to that
    range and highlighted in the text;
  - Send to nexus puts it in the conversation, as on decks;
  - the model's comments arrive the same way, with severity rings on the
    margin cards;
  - a comment with `replace_text` operations is a **suggestion**, drawn inline
    as a tracked change (deleted text struck in the error color, inserted text
    underlined in accent2) with Accept and Reject on its card;
  - Accept all and Reject all appear in the panel header;
  - other operations show as a preview diff on the card.
- **Page preview** swaps the editor for the core-rendered pages (read-only,
  with page numbers). Export offers DOCX, PDF, Markdown and HTML, with "into
  template" when the doc follows a template design.
- **Design panel** (docs and decks):
  - the current design's colors, fonts and styles;
  - "follow a different design" and the space's templates;
  - an "apply update" button when the design's revision moved;
  - for decks, the layout gallery, from which "New slide" inserts a layout.

## The link (version 2)

Version 2 renames `deck_path` to `path` (accepting `deck_path` on input) and
adds `kind: "deck" | "doc"` on the envelope. A doc comment carries `block` and
`range`/`quote` in place of `slide`/`point`. `Focus` for a doc carries the block in
view and the selected text. Nexus and studio ship together, so both sides move to
version 2 at once, and a version-1 peer gets a clear "update nexus-studio" error.
The messages and the doc comment text are in `docs/studio.md` (*The link*), the
window in *The doc editor*.

In the conversation, a doc comment reads `Comment on docs/q3.doc.json · b4
"Ship the docs canvas": text`, with the thread id, the doc revision and the
anchored block's text, so the model can edit without a `read`. Doc comments
don't attach a page image; the text is enough.

## Phases

1. **P1, design system and doc core.**
   - `design/` (schema, defaults derived from the scale, built-in slide
     layouts, validation, lint) and `doc/` (schema, validation, operations,
     Markdown in, revisions, history, comments);
   - the `canvas` tool with doc actions, `add_slide {layout}`, `apply_design`
     and the design lint, all within the token ceiling;
   - prompts and goldens.
2. **P2, pages and files.**
   - doc layout and pagination, the page SVG renderer, PNG view;
   - `krilla` vector PDF for docs and decks;
   - DOCX, Markdown and HTML export; DOCX/DOTX and Markdown import;
   - `nexus doc …` CLI parity with `nexus deck …`.
3. **P3, the studio doc editor (done).** Doc mode, block editing, comments with
   tracked-change suggestions, outline, page preview, export menu, the Design
   panel, and link version 2 with the TUI wiring.
4. **P4, templates.**
   - upload paths and design extraction from PPTX/POTX and DOCX/DOTX, with
     layouts from slide layouts;
   - export into template packages;
   - the Design panel's templates.

Each phase lands on its own, with `scripts/check.sh` green and tests for:
- schema round trips;
- every operation and its errors;
- lint;
- pagination rules;
- export bytes (determinism, OOXML that opens: checked structurally, plus
  sample files the user opens in Word/PowerPoint/Keynote);
- import fidelity on fixtures;
- link messages;
- studio snapshot tests where they exist.
