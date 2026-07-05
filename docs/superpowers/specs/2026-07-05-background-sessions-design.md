# Background Sessions — Design

Date: 2026-07-05
Status: approved

## Problem

Stream state is global on `App` (`streaming`, `stream_rx`, `thinking_text`, …) with no
record of which session started it. `confirm_session` and space switches don't check
`is_streaming()`, so a response that finishes after a switch is written to whichever
session is active at that moment — the wrong one — and space switches can drop it
entirely (`session = None`). Users also can't leave a slow response running and browse
other sessions.

## Goal

One global stream, backgrounded: switching sessions or spaces leaves the in-flight
response running; it always lands in the session that started it; completion notifies
via the status bar and a marker in the session picker. Sending a new message anywhere
stays blocked until the stream finishes (explicitly chosen over per-session
concurrency).

## Design

### State & routing

- `App.stream_session: Option<(String, String)>` — `(session id, title)` of the stream's
  origin. Set in `send_message` immediately after the session row is ensured (a session
  always exists by stream start); cleared in `finish_stream`.
- `App.unread: HashSet<String>` — session ids with a response that finished while the
  user was elsewhere. In-memory only; a restart forgets it (the response is in the db).
- `on_stream_event` `ToolCall` arm: the tool_call row is persisted to
  `stream_session.id` (not the active session). It's pushed onto `self.messages` only
  when the active session is the stream's session.
- `finish_stream`: `add_assistant_message` targets `stream_session.id`. If the user is
  viewing that session, push to `messages` as today. Otherwise `unread.insert(id)` and
  set status `✓ response ready in: {title}`. `context_total` is updated only when
  viewing (it describes the active conversation).
- Session switch, `/new`, and space switch do not touch stream state. Switching back to
  the streaming session reloads messages from the db (persisted tool_call rows land
  correctly) and the live tail resumes rendering. Opening a session removes it from
  `unread`.

### UI

- The streaming tail + spinner in the transcript render only when
  `active session id == stream_session.id`.
- Input hint while a stream runs elsewhere: dim `⟳ streaming in: {title}` (replaces
  "…working (Esc to stop)", which shows only when viewing the streaming session).
- Session picker glyphs: `⟳` on the session with the in-flight stream, `●` on unread
  sessions.
- Esc stops the stream only when viewing its session; elsewhere Esc keeps its normal
  clear-composer behavior.

### Kept semantics

- `send_message` blocks globally while streaming, with the origin named:
  `wait — response still streaming in: {title}`.
- Compaction's `is_streaming()` guard stays as is.

### Edge cases

- Stream error: same away-path — unread marker + status names the session.
- Deleting the session that is streaming aborts the stream first (otherwise it writes
  into a deleted session row).
- Streams never outlive the process; quitting aborts as today.

## Testing

- Finish-while-away: response row lands in the origin session, `unread` gains the id,
  status is set, and the active session's `messages` are untouched.
- Mid-stream tool_call while away persists to the origin session only.
- Switching to an unread session clears its marker.
- Deleting the streaming session aborts the stream.

## Out of scope

- Per-session concurrent streams (revisit if the global block chafes).
- Persisting unread markers across restarts.
- Desktop/bell notifications.
