# Nexus Chat web client

Build the SolidJS client and serve it directly from `nexus host`:

```sh
cd web
npm ci
npm run build
cd ..
cargo run -- host
```

Open `http://127.0.0.1:8643` and enter the host bearer token. The client defaults
to its own origin and discovers the host at `/.well-known/nexus`. No Vite server
is needed in production. `nexus host` serves `web/dist` relative to its working
directory; for an installed binary or service set `NEXUS_WEB_DIR` to an absolute
path containing the built `index.html` and `assets/` directory. Distribute that
directory alongside the binary; the build is not embedded in the Rust executable.

The public web assets and discovery endpoint contain no credentials. API calls
and SSE still require a bearer token. Tokens stay in `sessionStorage` by default;
remembering access across browser sessions is opt-in. Generated `/apps/` pages
receive a CSP sandbox with an opaque origin, preventing them from reading the
client's browser storage. App scripts, forms and downloads remain enabled;
apps should use their capability-scoped KV API instead of browser localStorage.

The workspace includes chat, conversations, research activity, files, media,
apps, management, usage, settings and sync. Spaces support listing, creation
and switching. Files support scoped upload (64 MiB limit, no overwrites),
verified downloads and image previews. Conversations support rename, deletion,
copy, Markdown export and context usage inspection.
Chat accepts image uploads and clipboard images up to 10 MiB; incognito image
attachments use temporary storage that is cleaned up with the conversation.

The Apps library supports browsing, registration, removal, launch, sandboxed
previews, builds and editing existing UTF-8 source files up to 1 MiB. Saves
reject stale revisions. Hidden files, symlinks and node_modules are excluded.
Research activity restores live stages and pending survey/plan gates after
reconnect, accepts steering and gate answers, and exposes reports and citations.
Management contains swarm rosters, skills and watches. Settings cover generation,
models, search, display, instructions, memory and provider login; API keys are
write-only and device login opens in the browser using a verification code.

Sync supports browser-mediated exchanges with another host and offline ZIP
bundles. Merges use the existing host conflict rules and verified blob transfer.
Peer tokens are held in memory unless access was explicitly remembered.
Provider/media integrations require the corresponding configured provider or
local tools; automated checks mock external services.

## Development and checks

```sh
cd web
npm ci
npm run dev          # proxies /v1 and discovery to 127.0.0.1:8643
npm run build        # TypeScript check and production build
npx playwright install chromium
npm test             # desktop/mobile smoke tests with mocked API and SSE
```

To use an existing Chromium installation:
`CHROMIUM_PATH=/usr/bin/chromium npm test`.

Run `scripts/check.sh` at the repository root for the Rust gate, plus
`npm --prefix web run build` and `npm --prefix web test` for web changes.
CI runs all three. Browser tests use mocked data and never contact a provider.

`VITE_NEXUS_URL` can override the default origin for development. Enter credentials
in the pairing form; do not bake real tokens into distributed builds using
`VITE_NEXUS_TOKEN`.

## Workspace API

All routes below require `Authorization: Bearer …`:

- `GET /v1/spaces`: spaces and the active space ID.
- `POST /v1/spaces?name=…`: create a space.
- `POST /v1/command` with `{"SwitchSpace":{"name":"…"}}`: switch spaces.
- `GET /v1/files?space_id=…`: file metadata (defaults to the active space).
- `PUT /v1/files?space_id=…&name=…`: upload a new file as a binary body.
- `GET /v1/sync/blob?space_id=…&name=…`: download verified file bytes.
- `POST /v1/attachments?space_id=…`: attach an image binary using the chat
  clipboard workflow, returning the Markdown reference for the composer.

URL-encode query values. Uploads capture the selected space ID so switching the
workspace during a request cannot send a file to the wrong space.

Apps API (bearer authentication required):

- `GET /v1/apps?space_id=…`: app names, capability launch paths and served directories.
- `GET /v1/apps/files?space_id=…&name=…`: bounded source-file catalog.
- `GET /v1/apps/source?space_id=…&name=…&path=…`: UTF-8 content and revision hash.
- `PUT /v1/apps/source?space_id=…&name=…&path=…`: save `{ "content": "…", "hash": "…" }` using the revision returned by GET.

Additional authenticated surfaces:

- `/v1/media`, `/v1/media/blob`, `/v1/media/action`: media catalogs, binary
  downloads, OCR, attachments, renaming and script editing/execution.
- `/v1/research/<session-id>` with `/answer`, `/steer`, `/stop`, `/citations`
  and `/context`; `/v1/research/start` starts a new project.
- `/v1/swarm`, `/v1/skills`, `/v1/watches`: management state and actions.
- `/v1/settings`, `/v1/settings/document`, `/v1/login`: configuration,
  revision-checked documents and provider enrollment.
- `/v1/sync/state` and `/v1/sync/bundle`: peer history and offline exchange.
- `PATCH`/`DELETE /v1/sessions/<id>`: rename or delete a conversation.

Script runs have a 30-second deadline and a 1 MiB limit per output stream.
They run outside the app actor so chat and SSE remain responsive.
