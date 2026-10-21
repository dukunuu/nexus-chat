# Design: connectors (MCP)

Status: design, 2026-10-21. Decided with the user: build on MCP; remote
(HTTP + OAuth) **and** local (stdio) servers, remote first; tools the server
marks read-only run freely, everything else asks for approval (in chat mode
too); secrets live in the OS keychain.

## Goal

Connect nexus to outside services the way Claude, ChatGPT and Cursor do:
add a connector (a URL for a hosted server, or a command for a local one),
sign in once, and its tools become available to the model in every space,
in chat and agent mode. Existing MCP configs from Claude Desktop, Claude
Code and Cursor import with one command. Later the `nexus host` shares the
same connectors with the web app.

## Protocol support

MCP's current revision is **2026-07-28**, which is stateless: no
`initialize` handshake and no `Mcp-Session-Id`; every request carries
`_meta` (`io.modelcontextprotocol/protocolVersion`, `clientCapabilities`,
`clientInfo`), servers implement `server/discover`, results carry
`resultType`, and server-initiated requests are replaced by multi
round-trip requests (`input_required`). Most deployed servers still speak
**2025-11-25 / 2025-06-18 / 2025-03-26** (initialize handshake, sessions).

nexus implements its own small client (no SDK dependency; the protocol is
JSON-RPC over two transports) that speaks both:

1. Call `server/discover`. If the server answers, use 2026-07-28 rules.
2. If the method is unknown (or the transport answers 4xx for it), fall
   back to `initialize` offering 2025-11-25 and accept the version the
   server picks among 2025-11-25, 2025-06-18 and 2025-03-26; send
   `notifications/initialized`; keep the `Mcp-Session-Id` it returns.
3. Treat a missing `resultType` as `complete`.

Transports: **Streamable HTTP** (POST; JSON or SSE response bodies; the
`Mcp-Method`/`Mcp-Name` headers for 2026 servers; no resumability — a
broken stream re-issues the request with a new id) and **stdio** (a child
process, newline-delimited JSON; stderr goes to a per-connector log). The
deprecated HTTP+SSE transport is not supported.

Features used: `tools/list` (cached by `ttlMs` when given, refreshed on
list-changed notifications and on reconnect; deterministic order),
`tools/call` (text, image, audio-as-note, resource links, embedded
resources, `structuredContent`), annotations (`readOnlyHint`,
`destructiveHint`, `idempotentHint`, `openWorldHint`, `title`). Later
phases add resources, prompts, and `input_required` elicitation. Roots,
sampling and logging are deprecated in 2026-07-28 and not implemented.

## Configuration

```toml
[connectors.linear]
url = "https://mcp.linear.app/mcp"        # remote
enabled = true

[connectors.github]
url = "https://api.githubcopilot.com/mcp/"
auth = "bearer"                            # token in the keychain

[connectors.files]
command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/Users/me/notes"]
env = { LOG_LEVEL = "warn" }               # values may be "keychain:<name>"

[connectors.linear.tools]
create_issue = "ask"                       # ask | allow | deny (overrides)
```

- Names are `[a-z0-9_-]{1,32}`; a connector's tools are exposed to the
  model as `<connector>__<tool>` (sanitized to the provider's tool-name
  charset, ≤ 64 chars, collisions get a numeric suffix).
- Secrets never appear in config.toml when a keychain exists: OAuth tokens,
  client credentials and bearer tokens are stored with the `keyring`
  crate under service `nexus-chat`, account `connector:<name>:<kind>`,
  keyed by the authorization server's issuer as the spec requires. Where no
  keychain is available, they fall back to config.toml (0600) with a
  one-time warning.
- `nexus connectors import` reads Claude Desktop
  (`claude_desktop_config.json`), Claude Code (`~/.claude.json` and
  project `.mcp.json`) and Cursor (`~/.cursor/mcp.json`) server entries,
  shows them, and adds the chosen ones (secrets in their `env`/`headers`
  move into the keychain).

## Authorization (remote)

OAuth 2.1 as the MCP authorization spec requires:

- Discovery: a 401 with `WWW-Authenticate: Bearer resource_metadata=…`
  (or the well-known path) → Protected Resource Metadata (RFC 9728) →
  authorization server metadata (RFC 8414 / OpenID discovery).
- Client registration, in order: a pre-registered client id from config;
  **Client ID Metadata Document** (client_id is an https URL serving
  nexus's client metadata — `token_endpoint_auth_method: none`, loopback
  redirect URIs; hosting it needs a public URL, see open questions);
  Dynamic Client Registration (RFC 7591, deprecated but still the most
  widely deployed) with `application_type: native`.
- Authorization code + **PKCE S256**, `resource` indicator (RFC 8707), a
  loopback redirect on `http://127.0.0.1:<random port>/callback` (RFC
  8252), the browser opened with `open`, a 5-minute wait with a clear
  "waiting for sign-in" state and cancel.
- Validate `state`, and a present `iss` against the recorded issuer before
  redeeming the code (RFC 9207). Credentials are keyed by issuer and never
  reused with a different authorization server.
- Refresh tokens rotate; a refresh failure marks the connector "sign in
  again" instead of failing tool calls silently.
- Token audience: tokens are only ever sent to the connector's own URL.

## Tools in the conversation

- **Definitions**: connector tools join the model's tool list after the
  built-in ones, in a stable order (connector name, then tool name) so the
  prompt prefix stays cacheable. Descriptions are capped (1 KB) and
  schemas size-limited; over-long or invalid schemas disable that tool
  with a status note.
- **Many tools**: when enabled connectors bring more than 30 tools, they
  are not all sent. A single `connectors` meta-tool lists and searches
  them (`search {query}` → names + one-line descriptions + schemas), and
  the model calls a connector tool by name through it
  (`call {tool, arguments}`); tools used in the session are promoted into
  the regular list (appended, to keep the cache prefix).
- **Approval**: a tool runs without asking only if it is
  `readOnlyHint: true` and not `destructiveHint`, or the user set it to
  `allow`. Everything else shows the approval panel (in chat mode too)
  with the connector, tool, a readable argument preview and the
  annotations; "allow this tool for the session" is offered, "always"
  writes `allow` to config. `deny` tools are not sent to the model.
- **Rug-pull protection**: the definition hash (name + description +
  schema + annotations) of every tool the user has allowed is remembered;
  if a server changes a tool's definition, the tool falls back to `ask`
  and the approval panel says the definition changed.
- **Results** are untrusted tool output: text capped like other tool
  results, images passed as vision input when the model supports images,
  `structuredContent` rendered compactly; nothing in a result can trigger
  another call without the model asking for it (which goes through the
  same approval).
- **Cards**: connector calls get a tool card with the connector's name as
  the verb target, the argument preview and the result.

## Surfaces

- `/connectors` popup: list (status dot: connected, needs sign-in, error,
  disabled), add (URL or command), sign in / out, enable / disable, tools
  list with per-tool allow / ask / deny, last error and the log path.
- CLI: `nexus connectors list|add|remove|login|logout|import|tools`.
- `nexus host` (later): connectors run on the host; the web app sees them
  through the host API with the same approval rules (approvals from the web
  follow `docs/web-editors.md` §3).

## Phases

1. **C1 — client and tools.** Protocol client (both revision families),
   stdio and Streamable HTTP transports with bearer tokens from the
   keychain, config + keychain storage, tool discovery/caching, model
   exposure with namespacing, the >30-tool meta-tool, approval policy with
   session allow and rug-pull protection, tool cards, `/connectors` popup
   (list, add, enable, tools policy), CLI list/add/remove/tools, import.
2. **C2 — OAuth.** Discovery, PKCE, loopback redirect, DCR and CIMD, iss
   validation, refresh, sign-in/out in the popup and CLI.
3. **C3 — beyond tools.** Resources (attach with `@`), prompts (as
   `/connector:prompt` commands), `input_required` elicitation as a
   composer-area form, the tasks extension for long-running calls, and
   host/web sharing.

## Open questions

- A public URL for the Client ID Metadata Document (e.g. a file served from
  the project's GitHub Pages). Until it exists, nexus uses DCR or
  pre-registered client ids.
- A curated directory of popular connectors (URL + auth notes) in
  `/connectors` → "Browse", or leave discovery to the user.
