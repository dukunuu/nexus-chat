# Design: web slide and document editors, and a secure way for the web to control the host

Status: design. The implementation follows the agent-mode run (after phase 16b).

## Goal

Two editors in the web UI (`web/`, Solid), both backed by `nexus host` and
both with model suggestions you accept or reject:

- **Decks**: create a slide deck from a prompt, an outline or a document;
  edit it visually (move, resize, restyle, reorder, speaker notes); ask for
  suggestions ("tighten slide 3", "make this a two-column layout"), which
  show as previews you accept or reject; export `.pptx` and PDF. Closer to
  Claude Design than to today's "a skill writes a .pptx with python-pptx".
- **Docs**: a Notion-like block editor (headings, lists, toggles, callouts,
  tables, code, images, embeds of decks and charts) over files in a space;
  suggestions appear as tracked changes with a short rationale, accepted
  or rejected one by one or all at once; comments; export `.docx`, `.md`,
  PDF.

And the precondition the user set: **the web may drive the host only through
a connection as strong as SSH**, never through a QR code or a shared token,
and deployments must not turn into remote-access tools.

## What exists today

| Piece | Today | Problem for this feature |
|---|---|---|
| Host auth | one shared bearer token (`host/api.rs` `authorized`), constant-time compared | anyone holding it has full control; it can't be scoped, expired per device or revoked per device |
| CORS | `Access-Control-Allow-Origin: *` | any page in the browser can call the host if it has the token |
| Remote access | `nexus host --tunnel` (quick or named Cloudflare tunnel) | the token is the only barrier on the public internet |
| Model web apps | `app` tool, served by `appserver.rs` under `/apps/<uuid>/`; the UUID is the capability | apps must never gain host control |
| Slides | none in core; `.pptx` text extraction only (`extract.rs`); decks come from skills or web apps | no editing, no suggestions |
| Docs | Markdown files in spaces, rendered read-only | no editing UI, no suggestions |

## 1. Connecting web to host: device keys, like SSH

The model is SSH's `authorized_keys` + `known_hosts`, with the browser as
the client.

### Keys

- **Host key.** On first run `nexus host` creates an Ed25519 key pair in
  the config dir (`host_ed25519`, mode 0600). Its fingerprint
  (`SHA256:…`, like `ssh-keygen -l`) is shown by `nexus host`, in the TUI
  (`/devices`) and on the web pairing screen.
- **Device keys.** Each browser generates a **non-extractable** ECDSA P-256
  key with WebCrypto (`extractable: false`, kept in IndexedDB), so script
  on the page can use the key but can never read it out. P-256 because
  WebCrypto Ed25519 support still varies across browsers. CLI clients
  (`nexus remote`) use an ordinary SSH key file (`~/.ssh/id_ed25519`), so
  an existing key and agent work unchanged.
- **Optional hardware step-up: passkeys (WebAuthn).** A device can also
  register a passkey (Touch ID, Windows Hello, a security key). It is
  required for approvals (§3) and admin actions. Passkeys are bound to the
  origin, so they need a stable hostname: a named tunnel or a local
  hostname, never a random quick-tunnel URL.

### `authorized_devices` (the `authorized_keys` file)

A plain file in the host's config dir, one device per line, editable by
hand like `authorized_keys`:

```
ecdsa-p256 <base64 public key> name="laptop chrome" scopes="read,edit:docs,edit:decks,agent:ask" expires=2027-01-20 added=2026-10-20
```

The host reloads it on change. It never syncs (it authorizes devices for
*this* host). `/devices` in the TUI lists, renames, rescopes and revokes
entries; revocation takes effect on the next request.

### Pairing (no QR secrets)

1. The browser opens the host URL, generates its key, and shows its
   **fingerprint** plus the host's fingerprint as the page received it.
2. It sends a pairing request: public key + requested name + scopes. The
   host holds **one** pending request at a time; it expires after 2
   minutes, and requests are rate-limited per IP.
3. On the host machine (TUI `/devices`, or `nexus devices pair`), the user
   sees the pending request's fingerprint and compares it with the one on
   the browser screen (the same check as SSH's "are you sure you want to
   continue connecting"), checks the host fingerprint shown in the browser
   against the TUI, picks scopes, and approves.
4. The device is appended to `authorized_devices`. Nothing secret ever
   travels in a QR code, a link or the pairing request.

Pairing is only possible from the host machine itself; a remote device can
never approve another device.

### Authenticating requests

- **Signed requests** (in the spirit of HTTP Message Signatures, RFC 9421):
  every mutating request carries `key-id`, a timestamp, a random nonce and
  a signature over `method | path | timestamp | nonce | sha256(body)`. The
  host rejects stale timestamps (±60 s), replays (a nonce cache for the
  window) and unknown or expired keys. Nothing replayable is ever stored
  in the browser.
- **Streams** (SSE): a short-lived (5 min) session token minted by a signed
  request, bound to the key id, sent only on the stream connection, and
  re-minted while the stream lives. Revoking the key kills its streams.
- **Host authenticity**: the host signs a challenge from the client on
  connect with its host key; the browser pins the host fingerprint after
  pairing (known_hosts) and refuses to talk if it changes.
- **CORS**: `*` goes away. Only the host's own origin (and configured
  origins) may call the API. Model-written web apps under `/apps/<uuid>/`
  are a different origin from the editor UI, or an `iframe sandbox`
  without `allow-same-origin`, and get no ambient credentials.
- **The shared bearer token** survives only for loopback use by the local
  CLI and existing integrations, and stops working for any non-loopback
  request once `remote_control` is not `local`.

## 2. Modes: off by default for deployments

```toml
[host]
remote_control = "local"   # off | local | paired
```

| Mode | Who can control the host | Editors over a tunnel |
|---|---|---|
| `off` | nobody over HTTP except read-only routes | read-only views |
| `local` (default for `nexus host` without a tunnel) | loopback only | not reachable |
| `paired` | paired devices, by scope | full, by scope |

`--tunnel` without `paired` forces read-only; enabling `paired` over a
tunnel needs a named tunnel and prints the risks once. Multi-user or
hosted deployments do not get the control surface at all: building with
`--features hosted` compiles out pairing and agent scopes.

## 3. Scopes: capabilities, not remote access

| Scope | Allows | Never allows |
|---|---|---|
| `read` | list and read spaces, docs, decks, sessions | anything that writes |
| `edit:docs` / `edit:decks` | create and edit doc/deck files in spaces, request and apply suggestions | files outside spaces, shell, the `fs` tool |
| `agent:ask` | start an agent or chat turn that the host runs (e.g. "fix slide 3", "make a deck from this doc") | approving its own gated calls |
| `approve` | answer approval prompts from the web | anything without a passkey step-up |
| `admin` | settings, models, keys, devices | — host-local only, never granted over a tunnel |

There is **no web scope for `shell` or raw `fs`**. Agent work requested
from the web runs in the host's own agent loop, with the same workspace
confinement, escalation lock, sandbox and Decider as the TUI. Its approval
prompts are answered on the host, or on the web only by a device holding
`approve` **and** completing a passkey assertion (user presence plus
verification) for that specific request. Destructive and outside-reaching
actions can't be allowed for the session from the web.

Every remote action is logged with the key id (who did what), and
`/devices` shows last-seen times.

## 4. The editors

### Shared: documents as data, suggestions as operations

- **Storage**: plain files in the space, so sync, backup and the existing
  file tools keep working. Docs are Markdown with a small block extension
  syntax (toggles, callouts, embeds) and front matter. Decks are a JSON
  file (`*.deck.json`) with a versioned schema.
- **Suggestions** are structured operations against a **version** of the
  file, anchored by content hashes (the same idea as the `fs` tool's
  hash-line edits), each with a one-line rationale:
  `replace(block_id, old_hash, new_content, why)`, `insert_after`, `delete`,
  `move`, and for decks `set_layout`, `restyle`, `add_slide`. Stale
  suggestions (the anchored content changed) are shown as outdated and
  can't be applied, like the stale-write guard.
- **A `suggest` tool** (model-facing) takes a file, an optional selection
  and an instruction, and returns operations without writing; applying
  them is a separate, user-initiated call. Suggestions are stored
  alongside the file (`<file>.suggestions.json`) with author (model/user),
  status and comments.
- **History**: every applied change set is a revision; undo/redo and "show
  changes since" work across sessions.

### Docs (Notion-like)

- Block editor in Solid with a slash menu, drag handles, keyboard-first
  editing, Markdown shortcuts, and paste from Markdown/HTML (sanitized with
  the existing DOMPurify).
- Suggestion mode: inline insertions/deletions, a margin card per
  suggestion with accept / reject / reply, "accept all", and "explain".
- "Ask" on a selection: rewrite, shorten, expand, fix tone, continue, or a
  free instruction, all producing suggestions rather than silent edits.
- Export: `.md` (native), `.docx` and PDF (§5).

### Decks (Claude Design-like)

- **Schema**: a deck has a theme (tokens: colors, fonts, spacing; reusing a
  design system where one exists), layouts (title, section, two-column,
  image+text, quote, chart, blank), and slides made of blocks (text, image,
  chart, shape, table, code) on a 16:9 grid with positions in grid units,
  plus speaker notes.
- **Editing**: a slide strip, a canvas with drag and resize snapping to the
  grid, a properties panel, and keyboard shortcuts. Rendering is HTML/CSS
  from the schema; the same renderer produces thumbnails and PDF.
- **Generation**: from a prompt, an outline or a doc → the model emits a
  deck JSON through a `deck` tool that validates it against the schema
  (bad JSON goes back to the model as a recovery-oriented error, as with
  the other table tools).
- **Suggestions**: previewed as a ghost overlay on the slide ("before /
  after" toggle); deck-wide suggestions ("make the narrative tighter")
  arrive as a reviewable set.
- **Export**: `.pptx` and PDF (§5).

## 5. Export

- **PDF**: print CSS from the same HTML renderer, through a headless
  browser only if one is available, otherwise the browser's own print
  dialog.
- **`.pptx` / `.docx`**: two options, decided in the first implementation
  brief:
  1. a sandboxed Python venv with `python-pptx` / `python-docx`, the way
     skills already run, which is mature and quick to build; or
  2. a small Rust writer for the subset the schema can express, with no
     Python, fully hermetic tests and more work.
  Start with (1) behind the same sandbox as skills; revisit if it becomes
  a maintenance burden.

## 6. Threats and answers

| Threat | Answer |
|---|---|
| Stolen host token over a tunnel | the token stops working off loopback once `remote_control` isn't `local`; devices use keys |
| XSS in the editor steals the device key | the key is non-extractable; an XSS can still sign while the page is open, so CSP is strict (no inline script), all model and user content is sanitized, and approvals need a passkey assertion the page can't fake |
| Model-written web app calls the host | different origin or opaque sandboxed iframe, no credentials, CORS locked |
| Replay of a captured request | timestamp window + nonce cache + body hash in the signature |
| Malicious pairing request | only one pending, 2-minute expiry, rate limit, fingerprint compared on the host, approval only from the host machine |
| Lost or compromised laptop | `/devices revoke`; keys expire; passkey needed for approvals |
| Host impersonation | host key pinned after pairing (known_hosts) |
| "It's a RAT" | no shell or raw fs scope exists; agent work is gated exactly as locally; hosted builds compile it out |

## 7. Phases

1. **Device keys and signed requests.** Host key, `authorized_devices`,
   pairing in TUI and web, request signing and verification, CORS lockdown,
   `remote_control` modes, `/devices`, an audit log. No editors yet: the
   existing web UI moves onto signed requests.
2. **Docs editor** on space files: block editor, revisions, `suggest` +
   apply, comments, Markdown export.
3. **Decks**: schema, renderer, editor, `deck` tool, suggestions, PDF.
4. **Export** to `.pptx` / `.docx`.
5. **`agent:ask` and `approve` from the web**, with passkey step-up.

Each phase passes `scripts/check.sh` plus the web Playwright tests.

## Open questions

- Should paired devices be per space (a laptop sees only the "work" space)?
  The scope format allows `edit:docs@work`; default is all spaces.
- Real-time co-editing (two devices editing one doc at once) needs a CRDT
  or OT layer; out of scope for the first version (last write wins per
  block, with a conflict notice).
- Which design system decks use by default, and whether to import themes
  from existing `.pptx` templates.
