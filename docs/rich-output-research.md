# Research: model-authored rich outputs

**Scope decision:** TUI only for now. Browser rendering, browser-only widgets
and live inline web-app previews are deferred; their research below is retained
as comparison material, not current delivery scope.

**Implementation:** the first TUI slice adds local Mermaid previews for tested
flowchart/sequence/state diagrams, bounded background rendering, terminal
protocol detection, fixed-height slots and copyable source/error fallbacks.
Structured script outputs and app screenshot capture remain follow-ups. Markdown
images are now parser-recognized so image-like text inside code remains literal;
existing image path-policy and cache-revision work is still outstanding.

**Model-authored interactivity:** the game-specific demo/commands have been
replaced by version-1 `nexus-ui` documents in assistant replies. The TUI advertises
this capability to models and renders themed text, button/artwork groups and
string inputs over bounded scalar state, expressions and transactional local
actions. Game rules are authored data, not frontend game implementations. Source
and the existing message persistence/transport remain unchanged; local state is
temporary. New panels activate after the reply completes, avoiding live-task
identity/replay ambiguities. Explicit response actions prepare a reviewable
composer draft, never an unseen user instruction or automatic model call.
See the [runtime](../crates/core/src/interactive.rs),
[TUI controller](../crates/tui/src/interactive.rs),
[capability/schema](../crates/core/assets/interactive-ui.md), and
[data-only game fixture](../crates/core/tests/fixtures/interactive/rps.json).
No browser, code interpreter, filesystem/network handler, timers or sidecar is
involved. General tool/script display manifests remain a separate follow-up.

**Validation:** `scripts/check.sh` passes (771 tests). Coverage includes
rasterization, fence/container
parsing, control-character safety, clipping, cache/concurrency bounds, stale
completions, viewport stability, all nine data-authored game outcomes, forms,
transaction rollback, action/expression budgets, source/copy fallbacks,
review-before-send, draft isolation and tiny/resized viewports. Real Kitty/iTerm2/Sixel QA and
an official-Mermaid visual-fidelity comparison are still pending. The native
renderer supports a subset, not a guarantee of complete Mermaid compatibility.
Audit-required `quinn-proto` was updated to 0.11.18 and `rustls` to 0.23.46 in
the lockfile; no advisory suppression was added. Audit still reports allowed maintenance warnings,
including the rasterizer's `rustybuzz`/`ttf-parser` font stack.

**Research baseline checked:** 2026-10-13, against repository commit
`fe77282983515b8b1c4e73c6934cf09167db53b3` and the primary sources below.

## Recommendation

Make Nexus a conversation with **displayable outputs**, not a conversation that
executes whatever HTML or code the model happens to print.

Use two complementary inputs:

1. **Ordinary Markdown** for prose, code, images, tables, and fenced Mermaid.
2. **Explicit, typed display outputs** for script-produced files, diagrams, and
   registered apps. These appear directly beside the producing tool call;
   the assistant should not have to repeat the result to make it visible.

Keep rendering in a deep TUI module for this first slice. Do not introduce a
shared core display seam until structured tool outputs require one; browser
support is not a reason to add it preemptively. Preserve the original source and
a text fallback. Do not require a particular model provider, a browser
installation, or a new generative-UI framework for basic use.

For the first implementation, investigate native Rust Mermaid rendering,
reuse the existing image/media and app surfaces, and add real terminal graphics
progressively. Browser chat can offer live sandboxed apps; terminal chat should
offer a launch card and, optionally, a screenshot. A terminal graphics protocol
is an image transport, not a browser engine. [S1][S2][S3][S4]

## 1. Architecture at the research baseline

The requested feature is an extension of existing work, not a blank slate.
This inventory describes the baseline commit above, before the first TUI slice.

| Surface | Current behavior | Missing behavior |
| --- | --- | --- |
| TUI code | Styled fenced blocks, language labels, raw-code copy tracking | Rich interpretation of selected fences, especially Mermaid |
| TUI images | Local Markdown image references become colored half-block rows, capped at 20 rows; click opens the system viewer | Sharp graphics, SVG rasterization, correct placement among text, streaming previews |
| TUI history | Cached `Line`s rendered through a `Paragraph`, with ownership/copy metadata and viewport-pinning tests | Reserved image regions and non-text display layout |
| Web Markdown | `marked` plus DOMPurify, restricted to HTML; iframe/style/form/object/embed are forbidden | Diagram rendering, syntax highlighting, typed output mounts |
| Web images | Relative image names fetched with bearer authentication and converted to object URLs | Stable output identity/revisions; remote-image privacy policy |
| Tool results | `ToolExecutor::run` returns `(String, String)`; completed calls persist arguments/result as JSON in a message | A separate, bounded display payload |
| Scripts | Model tools run scripts and format stdout/stderr/exit status into text | Explicit registration of plots, tables, and other produced files |
| Web apps | Local app creation/building, UUID capability links, library preview iframe, source editing | App cards/previews directly in the conversation |

Evidence: [R1][R2][R3][R4][R5][R6][R7][R8].

Important details for implementation:

- TUI images are scanned separately from Markdown and placed before the answer
  body, not at their actual position in the prose. The scanner is string-based
  and joins the supplied reference to the files directory. A richer parser
  should distinguish real images from image-looking text inside code and apply
  explicit path policy before loading. [R2]
- The TUI image cache is keyed by `(path, width)`, not content revision. An
  overwritten output can therefore remain stale. [R3]
- The current `image` dependency enables PNG/JPEG/WebP, despite image catalogs
  recognizing additional extensions; SVG recognition does not imply SVG
  rendering. [R8][R9]
- Tool results are capped at 8,000 characters, with a larger aggregate batch
  cap. Display metadata must not be smuggled into text that is then truncated.
  Deduplication and batch behavior must remain intact. [R5]
- Provider stream events, persisted tool-call JSON, the host wire mirror, and
  web draft types all currently carry text results. Adding a display only to
  the live frontend would lose it on reconnect. [R6][R10]
- Model-run space scripts currently have a 120-second deadline. The separate
  web media execution path documents a 30-second deadline and 1 MiB per output
  stream. Do not mistake these for one unified execution contract. [R5][R11]
- Host `/apps/<uuid>/` responses enforce a CSP sandbox with an opaque origin,
  including when launched in a new tab. The standalone localhost app server
  does not currently emit the same CSP. Reuse the host policy deliberately;
  do not assume every app-serving route has identical protection. [R12][R13]
- File sync transfers manifest-backed files by space/name/hash. New durable
  display assets should use that mechanism; a new arbitrary artifact directory
  would not automatically gain sync support. [R14]

## 2. Desired experience

These are proposed product behaviors, not existing guarantees.

| Model/script produces | Conversation displays | Fallback/actions |
| --- | --- | --- |
| A Rust/Python/etc. fence | Highlighted code with language and copy | Original source; never auto-run |
| A completed `mermaid` fence | Diagram in its original position | Source toggle, error, export/open |
| A plot saved as PNG/SVG | Image with caption, directly on tool completion | Open full size, download, text description |
| Structured tabular data | Bounded table | Raw data/download, truncated-row indicator |
| stdout/stderr | Collapsible output with exit status | Full saved log if separately retained |
| A registered app | Named app card | Web: user-opened live preview; TUI: open in browser |
| An optional app screenshot | Static preview with revision/time | Explicitly labeled non-interactive; refresh/open |

Diagrams and useful images should remain visible even when verbose tool logs
are collapsed. Keep logs inspectable, but do not hide the actual result under
“10 tool calls.”

An `html` fence should stay code. Executable HTML belongs in an explicitly
published, sandboxed app, not an accidental preview inside the trusted chat DOM.

## 3. Rendering options

### Diagrams

**Official Mermaid in the web client** is the conservative compatibility
choice. It supports explicit rendering to SVG and syntax validation. Configure
`startOnLoad: false`, strict security, host-controlled configuration, and bounded
text/edge counts. Render completed fences, not every partial token. Bundle the
library locally rather than loading diagrams/fonts from a CDN. [S1][S5]

Do not insert generated SVG without a separate output policy: the current
Markdown sanitizer deliberately excludes SVG. DOMPurify supports SVG profiles
and warns that later transformations can invalidate an earlier sanitization
pass. One simpler first display is a generated SVG blob in an `<img>`; browser
image contexts disable scripting and restrict external resources, unlike SVG
opened as a document. This is additional protection, not a reason to skip input
validation or secure download/serving headers. [R4][S6][S7]

**Native Rust Mermaid is worth a compatibility spike.** The published
`mermaid-rs-renderer` 0.3.1 exposes `render`/`render_strict` to SVG and an optional
PNG feature backed by resvg/usvg. Its README advertises 23 diagram kinds, but
explicitly says output may not match official Mermaid and calls the project
early-development software. [S2]

This fits the local-first Rust app better than making Chromium mandatory.
However, “supports a diagram kind” does not establish full syntax/layout
compatibility. Test nested subgraphs, edge labels, sequence diagrams, ER
relationships, themes, invalid syntax, Unicode, and large inputs. Treat the
project's speed claims as author benchmarks, not measurements of Nexus.
The published 0.3.1 source was checked separately from GitHub master; newer
master-only features are not assumed available. [S2]

**Official Mermaid CLI** takes Mermaid input and emits SVG/PNG/PDF. Its
implementation uses Puppeteer to launch a browser. This is a good optional
high-compatibility renderer, but adds Node/browser installation, startup and
process lifecycle work. Invoke a configured, installed executable, not `npx`
that silently downloads software while displaying a message. Its JavaScript
interface is explicitly not covered by semver; prefer the documented executable
contract if integrating it. [S3]

**SVG is not Mermaid.** resvg rasterizes static SVG; it does not parse Mermaid,
execute JavaScript, or supply HTML layout. Official Mermaid can generate HTML
`foreignObject` labels. Disable HTML labels when pursuing a static SVG pipeline
and verify actual output across diagram types rather than assuming identical
browser/resvg results. [S3][S5][S8]

**ASCII/Unicode diagrams** offer readable labels without graphics support.
`beautiful-mermaid` is a first-party example of SVG and terminal-text output,
with a smaller advertised diagram set. It is TypeScript, so adopting it for the
Rust TUI would introduce a runtime or a port. Useful comparison and potential
later fallback, but not a reason to add Node to the minimum install. [S9]

| Approach | Best fit | Main tradeoff | Recommendation |
| --- | --- | --- | --- |
| Official Mermaid JS | Browser chat | JS bundle and safe SVG integration | Web compatibility baseline |
| `mermaid-rs-renderer` | Core/TUI/local exports | Young implementation; parity unknown | Spike first; adopt only for tested support |
| Installed Mermaid CLI | Optional faithful raster output | Browser/Node dependency and process management | Optional fallback, not automatic installation |
| ASCII/Unicode renderer | Non-graphics terminals | Reduced diagram set and added implementation | Later; source/open fallback first |

If the native renderer passes the supported corpus, core can produce the same
SVG/PNG representation for both clients. Otherwise, web uses official Mermaid
while TUI uses the tested native subset or an optional installed CLI. Never
silently present an unsupported diagram as if it rendered correctly.

### Terminal images

`ratatui-image` handles Kitty, iTerm2, Sixel and half-block fallback, including
protocol/font-size detection. Its documentation specifically addresses the
interaction between graphics and an immediate-mode TUI. It also supplies
background resize/encoding examples and a sliced-image example for partial
visibility. [S4]

As checked, stable **11.1.0** declares Rust 1.86+, ratatui `^0.30.1`, and image
`^0.25.6`, compatible at the manifest level with Nexus's versions. This is not
a completed compile/audit check. Defaults now include dynamic libchafa linking;
use explicit features with defaults disabled to avoid adding an unexpected
system-library requirement. Do not copy a dependency choice from prerelease
master without checking the published version. [R9][S4]

Recommended progression:

1. Keep half-block/text fallback and improve source placement/path validation.
2. Add a dedicated full-size image/diagram viewer using `ratatui-image`.
3. Reserve rectangles inside history, then paint only visible image regions
   after the text pass; test clipping, overlap and cleanup before enabling
   inline graphics broadly.

A widget cannot simply replace a `Span`: the existing history is a line cache.
Kitty image placement/deletion and Unicode placeholders, and iTerm2's chunking
constraints, explain why printing `imgcat`/raw escape sequences from a tool is
not a safe shortcut. Let the TUI graphics adapter own protocol output. Test
actual terminals and multiplexers; detection is not proof of correct rendering.
[S4][S10][S11]

### Script outputs and charts

Borrow **Jupyter's MIME-bundle idea**, not its kernel/process stack: one result
can have multiple representations, and each frontend chooses a supported one.
Jupyter also keeps stdout/stderr distinct from rich `display_data`. [S12]

For example, a script plot can offer PNG plus a text description. Matplotlib
already exports PNG/SVG through `savefig`; no notebook infrastructure is needed.
A small helper can save to a controlled run directory and publish a manifest.
Do not guess that every JSON-looking line of stdout is a display instruction,
or scan the entire space to decide which files a particular run produced.
[S13]

A proposed minimal script contract:

- Nexus supplies a per-run output directory and manifest destination.
- An optional helper writes allowlisted display entries; regular stdout remains
  regular stdout.
- After execution, Nexus validates the manifest, verifies the files, assigns
  output identities/hashes, and publishes displays alongside logs/status.
- Other languages can emit the same manifest without importing Python tooling.
- Only explicitly registered files are retained or shown. Failed runs may still
  expose valid partial outputs, clearly marked with the failed exit status.

A Python virtualenv manages dependencies; it is **not** an OS/filesystem/network
sandbox. Display validation must not be described as making existing script
execution safe to run without permission.

Start charts as plot images. **Vega-Lite** is a later declarative option for
interactive browser charts; Vega-Embed accepts specifications and provides
SVG/canvas renderers and cleanup. It also accepts URLs/loaders and can access
external data immediately when a view is constructed. If added, require
bounded inline data, a restrictive loader from construction time, controlled
embed options, and cleanup; “JSON rather than JavaScript” is not sufficient
isolation. [S14]

### Web app previews

Reuse the existing registered app and iframe mechanism. A display should refer
to an app UUID that core resolves and verifies for the originating space, not
to an arbitrary URL supplied by the model. [R7][R12]

Start previews on explicit user action. A web app may perform network requests,
so a preview is active content, not merely another image. Preserve the opaque
origin, no-referrer behavior, and server-side sandbox policy. Do not add
`allow-same-origin` next to `allow-scripts` for app content on the host origin:
the HTML standard explains the sandbox escape that combination permits. Also,
iframe sandboxing alone is not an outbound-network allowlist. [S15]

For the TUI, a screenshot is an optional separate renderer. Playwright supports
page/element screenshots and request interception, but capture entails running
JavaScript in a browser. It needs its own supervised, credential-free process,
resource limits and network policy. Restrict capture targets to validated apps;
block external traffic/redirects and local-network access by default, including
service workers and other channels. Request interception alone is not an OS
network sandbox. No screenshots or browser sidecars should run from ordinary
hermetic tests. [S16]

## 4. The seam to implement

This is an illustrative interface, not a finalized schema:

```text
ToolOutput {
  context_text: String,       // capped provider-facing result/receipt
  status: String,
  displays: Vec<Display>,     // separately validated and bounded
}

Display {
  id: UUID,
  title: String,
  fallback: String,
  representations: [...],    // source, image/file, table, or registered app
}
```

A representation is an allowlisted typed value, not an unrestricted MIME-to-HTML
escape hatch. Large binary data is referenced by space/file/hash, not copied
into messages or prompts. Space/session/run provenance is assigned by the
executor, not accepted on faith from the model.

The deep display module owns validation, representation selection metadata,
asset registration, source preservation and export fallbacks behind a small
interface. The seam belongs between tool/content production and frontend
rendering, not inside provider-specific response parsing. The immediate adapter is TUI only; web is deferred. Do not introduce a renderer
interface just for a hypothetical second adapter. Core must remain free of
ratatui and DOM types.

Separate two representations deliberately:

- **Model context:** capped text and concise output receipts; image inputs only
  through the existing vision-aware path when appropriate.
- **User display:** source/data plus authenticated asset references, rendered
  according to client capabilities.

Do not change the provider's actual tool-call/replay semantics or put image
bytes/generated HTML into the context to make the UI work.

### How models publish

Keep familiar Markdown for assistant-authored diagrams and code. Existing
media/app/script tools should automatically attach their known outputs.
If models also need to publish an existing file/table/diagram independently,
a small `display` tool is a candidate—not separate tools for every widget.
It validates and registers content; it does not execute code or fetch arbitrary
URLs. This tool choice should be settled after the first renderer spike.

### Persistence and transport

For tool outputs, optional `displays` inside existing stored tool-call JSON
could avoid a new messages-table migration for the first slice. That still
requires coordinated changes to the executor, stream events, host wire types,
web drafts, reconnect loading, exports and all provider loop consumers. Older
messages without the field render exactly as today. [R5][R6][R10]

Completed Markdown diagrams can remain source-derived displays with disposable
render caches. Persist actual script assets in manifest-backed space files;
incognito outputs require temporary storage and explicit cleanup, not ordinary
durable registration. [R14]

Use stable output UUIDs/revisions rather than screen line numbers. The current
public `Message` type has no message ID and the host stream deliberately omits
the provider call ID; do not assume either is available as a frontend identity.
Add display identity explicitly and keep it through persistence/reconnect.
[R6][R10]

Batch execution must merge child displays while applying display-count/byte
limits separately from text caps. Read-only tool deduplication should preserve
references to already-published outputs instead of erasing the visible result.

## 5. Streaming, performance and safety requirements

- Parse block structure, not regex guesses across the whole answer. Preserve
  code fences, image positions, raw source and incomplete streamed content.
- Show source/a stable placeholder while a Mermaid fence is incomplete. Render
  on completion; treat final invalid syntax as a local error with source intact.
- Decode/layout/rasterize/encode outside draw functions and outside the host
  actor. Cache by content hash, renderer version, theme, size and font policy.
- Ignore stale async results after a source, revision, session or space changes.
  Revoke object URLs and release terminal graphics on replacement/exit.
- Reserve known output dimensions. New content above a scrolled viewport must
  not move what the user is reading. Text-prefix anchors alone cannot identify
  an image: use output identity and an offset within its reserved region.
- Bound source/data bytes, output count, decoded pixels, dimensions, render
  concurrency and execution resources. An async timeout around a blocking Rust
  render does not forcibly stop that render; enforce input complexity limits or
  use a killable worker when a hard deadline is required.
- Validate canonical paths, containment and symlinks before reads; do not rely
  on a lexical `join`/`starts_with`. Verify assets by hash and decode the claimed
  format rather than trusting filename extensions.
- No arbitrary `file://`, remote embeds, script tags or terminal escape
  passthrough. Remote images currently pass through the web Markdown path;
  define a consent/proxy policy rather than expanding automatic network access.
- Never expose host/provider tokens in asset URLs, screenshots, generated apps,
  events, model context or iframe messages. Public app UUIDs are capabilities;
  private conversation outputs must not accidentally acquire public URLs.
- A rich output is untrusted content, including when generated by a local model
  or produced by a script the model wrote. Source toggles, alt text and useful
  text/export fallbacks are part of the feature, not debugging extras.

These are proposed requirements. Existing viewport tests, authenticated media
fetches and host app isolation supply regression surfaces, not proof that the
new display path is secure. [R1][R4][R12]

## 6. Why not adopt a whole generative-UI framework immediately?

Two relevant first-party approaches:

- **MCP Apps:** tools declare UI resources; the host loads a sandboxed interface
  and exchanges tool data/actions over a bridge. Useful later for third-party
  interactive tool UIs, but Nexus currently has its own tool executor and app
  serving model. The specification includes CSP, tool visibility, lifecycle and
  messaging rules—not merely “put a tool result in an iframe.” Its recommended
  separate sandbox architecture must not be confused with granting same-origin
  access to Nexus-hosted model HTML. [S17]
- **A2UI:** declarative UI descriptions map to a host-controlled catalog of
  trusted widgets. A good direction if we want generated forms, buttons or
  approval dashboards. It would still require Nexus-specific TUI/Solid
  renderers and action authorization. Its catalog idea is useful now; adopting
  the full protocol is unnecessary for code, plots, diagrams and app previews.
  [S18]

Recommendation: implement a small output vocabulary first. The delivered
`nexus-ui` subset now addresses native form/game state without adopting either
full protocol; revisit standard bridges/catalogs for third-party UI resources or
cross-client interoperability. Neither requires replacing the provider/tool stack.

## 7. Proposed delivery order

### Spike — settle the diagram renderer

Without altering production behavior, evaluate published native Mermaid and
terminal image support using a small fixture corpus. Check build/audit cost,
render correctness, Unicode/font behavior, malformed/large inputs, and clipping.
The native spike rendered flowcharts, nested subgraphs, sequence/class/ER
samples and Unicode labels. It also revealed persistent font-cache writes in
some renderer paths, so this first slice supports only flowchart/sequence/state
with fast text metrics. No fidelity comparison against official Mermaid or
manual terminal compatibility QA has been completed.

### First slice — diagrams are actually visible

- Parse Mermaid fences and preserve ordered Markdown/image positions.
- Display rendered diagrams with source/error/open fallbacks.
- Detect terminal graphics once and retain half-block fallback.
- Keep source-copy affordances; defer a full-size image viewer to a follow-up.
- Prove streaming/scroll behavior before introducing generalized output state.

### Second slice — tools publish useful results

- Add typed output metadata across executor, events, persistence and host wire.
- Publish generated images and validated script output manifests directly.
- Keep logs collapsed without hiding plots/tables.
- Handle batches, old transcripts, incognito cleanup and asset sync/reconnect.

### Third slice — apps in the conversation

- Add registered app cards with explicit launch actions from the TUI.
- Optionally add screenshot capture for TUI previews as an opt-in capability.
- Do not attempt interactive HTML inside the terminal.
- Keep browser-client changes and declarative browser widgets out of scope.

## 8. Verification plan

**Core, hermetic:** block parsing, incomplete fences, unknown formats,
malformed manifests, output budgets, cross-space references, traversal/symlinks,
hash revisions, batch propagation, old-message replay, text caps/dedup,
incognito cleanup, export fallback, and provider-context separation.

**TUI `TestBackend`:** source/error/half-block states, diagrams between prose
sections, tiny/narrow layouts, resize, copy-source, a display growing above the
viewport, tool completion while scrolled up, and streaming-to-stored transition.
Do not require a real terminal protocol in automated unit tests.

**Web Playwright, mocked host:** completed Mermaid diagrams, invalid/incomplete
source, lazy loading, authenticated blobs, stale-result cancellation, revision
updates, tool displays outside collapsed logs, sandbox storage isolation,
no token-bearing URLs, safe app selection, export and reconnect behavior.

**Manual QA:** Kitty/Ghostty, iTerm2, a Sixel terminal and a no-graphics terminal;
tmux/SSH; fast scrolling, partial images, popup overlap, space/session switches
and image deletion. Protocol support varies by terminal/version. [S4]

Run `scripts/check.sh`; if touching web, also run `npm --prefix web run build`
and `npm --prefix web test`. Real browser rendering/sidecars belong in explicit
optional checks, not the ordinary network-free Rust suite. [R11]

## 9. Decisions before implementation

1. **Priority — decided:** TUI only. Leave `web/` and browser behavior unchanged.
2. **Minimum installation:** retain the Rust-only runtime baseline? Recommended:
   yes; make Chromium-dependent diagrams/screenshots optional.
3. **Mermaid fidelity:** is a clearly documented tested subset acceptable if
   native rendering is materially simpler? Decide from the spike, not claims.
4. **Preview trust:** recommended default is static content automatically,
   active app previews on explicit user action; outbound access requires policy.

## Sources

### Repository evidence

Paths are relative to this note; line locations refer to the checked commit.

[R1]: ../crates/tui/src/ui/markdown.rs
[R2]: ../crates/tui/src/ui/cards.rs
[R3]: ../crates/tui/src/history_cache.rs
[R4]: ../web/src/MarkdownContent.tsx
[R5]: ../crates/core/src/tools.rs
[R6]: ../crates/core/src/app/chat.rs
[R7]: ../web/src/Apps.tsx
[R8]: ../crates/core/src/app/images.rs
[R9]: ../Cargo.toml
[R10]: ../crates/core/src/host/wire.rs
[R11]: ../web/README.md
[R12]: ../crates/core/src/host/api.rs
[R13]: ../crates/core/src/appserver.rs
[R14]: ../crates/core/src/sync.rs

- **R1–R3:** TUI Markdown, card builders and history cache. Also inspect
  [`history.rs`](../crates/tui/src/ui/history.rs), especially its pinning tests.
- **R4:** authenticated image handling;
  [`markdown.ts`](../web/src/markdown.ts) contains the sanitizer policy and
  [`App.tsx`](../web/src/App.tsx) contains tool-result presentation.
- **R5:** `ToolExecutor` around line 460, script execution around line 1161,
  caps around line 3433, command-output formatting around line 3850.
- **R6:** persisted tool calls around line 520; model-history replay around
  line 1334. [`db.rs`](../crates/core/src/db.rs) defines `Message` around line 103.
- **R7–R9:** app preview, image recognition and enabled dependency features.
- **R10:** `WireStreamEvent` around line 101;
  [`provider/mod.rs`](../crates/core/src/provider/mod.rs) defines core events.
- **R11:** web capabilities and execution/check contracts. Also inspect
  [`host/media.rs`](../crates/core/src/host/media.rs) for scoped file validation.
- **R12–R14:** host CSP around line 1780, standalone response headers around
  line 405, sync file manifests/blob verification around lines 1589–1720.

### External primary sources

Version observations are a point-in-time research snapshot, not dependency
pins. Development-branch documentation may move; implementation must recheck
published artifacts. No external project benchmark is treated as a Nexus result.

[S1]: https://mermaid.js.org/config/usage.html
[S2]: https://docs.rs/crate/mermaid-rs-renderer/0.3.1/source/src/lib.rs
[S3]: https://github.com/mermaid-js/mermaid-cli
[S4]: https://github.com/ratatui/ratatui-image/tree/v11.1.0
[S5]: https://github.com/mermaid-js/mermaid/blob/master/packages/mermaid/src/schemas/config.schema.yaml
[S6]: https://github.com/cure53/DOMPurify
[S7]: https://developer.mozilla.org/en-US/docs/Web/SVG/Guides/SVG_as_an_image
[S8]: https://github.com/linebender/resvg
[S9]: https://github.com/lukilabs/beautiful-mermaid
[S10]: https://sw.kovidgoyal.net/kitty/graphics-protocol/
[S11]: https://iterm2.com/documentation-images.html
[S12]: https://jupyter-client.readthedocs.io/en/stable/messaging.html#display-data
[S13]: https://matplotlib.org/stable/api/_as_gen/matplotlib.pyplot.savefig.html
[S14]: https://github.com/vega/vega-embed
[S15]: https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-sandbox
[S16]: https://playwright.dev/docs/screenshots
[S17]: https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx
[S18]: https://github.com/google/A2UI

- **S1/S5:** Mermaid rendering/parse/security and host-only `secure` config.
  npm's [`latest` metadata](https://registry.npmjs.org/mermaid/latest) reported
  12.1.0 with Node `>=22.12.0` when checked.
- **S2:** published native renderer 0.3.1 source. Also checked its
  [published README](https://docs.rs/crate/mermaid-rs-renderer/0.3.1/source/README.md)
  and [manifest](https://docs.rs/crate/mermaid-rs-renderer/0.3.1/source/Cargo.toml.orig).
- **S3:** official CLI README and
  [browser-launch implementation](https://github.com/mermaid-js/mermaid-cli/blob/master/src/index.js).
- **S4:** stable image-widget README,
  [manifest](https://github.com/ratatui/ratatui-image/blob/v11.1.0/Cargo.toml) and
  [sliced example](https://github.com/ratatui/ratatui-image/blob/v11.1.0/examples/sliced.rs).
- **S6–S8:** sanitizer guarantees/limitations, browser SVG image-context rules,
  and static SVG rasterization.
- **S9–S11:** alternative text diagrams and terminal protocol specifications.
- **S12–S13:** MIME representations vs streams, and direct plot-file export.
- **S14:** chart embedding and cleanup; also checked
  [Vega View loader timing/export](https://vega.github.io/vega/docs/api/view/).
- **S15–S16:** iframe sandbox semantics, screenshot support and
  [request interception limitations](https://playwright.dev/docs/network).
- **S17–S18:** interactive tool-UI resources/bridge and declarative widget
  catalogs. These are comparisons, not proposed dependencies.
- **Code highlighting:**
  [Highlight.js official usage](https://github.com/highlightjs/highlight.js)
  documents explicit-language highlighting and selective language imports;
  an optional browser enhancement, not an execution engine.
