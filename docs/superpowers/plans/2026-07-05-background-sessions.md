# Background Sessions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A streaming response keeps running when the user switches session or space, always lands in the session that started it, and notifies (status bar + session-picker markers) when it finishes.

**Architecture:** Tag the existing global stream with its origin `(session id, title)` captured at stream start. All db writes route to the origin; UI renders the live tail only when viewing the origin session; finishing elsewhere sets a status message and an in-memory unread marker. One global stream — sending anywhere stays blocked while it runs.

**Tech Stack:** Rust, ratatui/crossterm TUI, tokio, rusqlite. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-05-background-sessions-design.md`

## Global Constraints

- No new crates.
- Tests use `Db::open_in_memory()` + a temp-dir `Space` (`std::env::temp_dir().join(format!("nexus-…-{}", uuid::Uuid::new_v4()))`) — never the `tempfile` crate.
- Commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Commits go straight to `master`, staging exact files.
- Run `cargo test` (full suite) before every commit; all tests must pass.

---

### Task 1: Stream origin state + `viewing_stream()`

Tag the stream with its origin session and add the unread set. Pure state — no behavior change yet.

**Files:**
- Modify: `src/app/mod.rs` (App struct fields ~line 460, init ~line 620)
- Modify: `src/app/chat.rs` (`start_stream`, ~line 189 where `stream_rx` is set)
- Test: `src/app/tests.rs`

**Interfaces:**
- Produces: `App.stream_session: Option<(String, String)>` (origin session id, title), `App.unread: std::collections::HashSet<String>`, `App::viewing_stream(&self) -> bool`. Later tasks rely on these exact names.

- [ ] **Step 1: Write the failing test**

Append to `src/app/tests.rs`:

```rust
#[tokio::test]
async fn stream_is_tagged_with_its_origin_session() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello");
    a.submit().unwrap();
    let sid = a.session.as_ref().unwrap().id.clone();
    assert_eq!(a.stream_session.as_ref().map(|(id, _)| id.clone()), Some(sid));
    assert!(a.viewing_stream());

    // Switch to a blank chat: still streaming, but not viewing it.
    a.new_session().unwrap();
    assert!(a.is_streaming());
    assert!(!a.viewing_stream());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test stream_is_tagged_with_its_origin_session 2>&1 | tail -5`
Expected: COMPILE ERROR — `stream_session` and `viewing_stream` don't exist.

- [ ] **Step 3: Implement**

In `src/app/mod.rs`, after the `stream_abort` field:

```rust
    /// Origin of the in-flight stream as (session id, title) — the response
    /// always lands there, even if the user switches away mid-stream.
    pub(crate) stream_session: Option<(String, String)>,
    /// Sessions holding a response that finished while the user was elsewhere.
    pub(crate) unread: std::collections::HashSet<String>,
```

In the `App::new` initializer, next to `stream_abort: None,`:

```rust
            stream_session: None,
            unread: std::collections::HashSet::new(),
```

In `src/app/mod.rs`, next to `is_streaming()`:

```rust
    /// True when the active session is the one the in-flight stream belongs
    /// to (untagged streams count as viewed — legacy/test paths).
    pub fn viewing_stream(&self) -> bool {
        self.is_streaming()
            && match (&self.stream_session, &self.session) {
                (Some((id, _)), Some(s)) => *id == s.id,
                (None, _) => true,
                (Some(_), None) => false,
            }
    }
```

In `src/app/chat.rs` `start_stream`, right before `self.stream_rx = Some(rx);`:

```rust
        self.stream_session = self.session.as_ref().map(|s| (s.id.clone(), s.title.clone()));
```

- [ ] **Step 4: Run tests**

Run: `cargo test 2>&1 | tail -3`
Expected: all pass (183+).

- [ ] **Step 5: Commit**

```bash
git add src/app/mod.rs src/app/chat.rs src/app/tests.rs
git commit -m "feat: tag the in-flight stream with its origin session

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Route stream results to the origin session + notify

The corruption fix: db writes target the origin, and finishing while away notifies instead of polluting the active transcript.

**Files:**
- Modify: `src/app/chat.rs` (`on_stream_event` ToolCall + Error arms ~lines 214–243, `finish_stream` ~line 261, `send_message` guard ~line 29)
- Test: `src/app/tests.rs`

**Interfaces:**
- Consumes: `stream_session`, `unread`, `viewing_stream()` from Task 1.
- Produces: away-finish behavior later tasks' UI reads: `unread` populated, `status` = `✓ response ready in: {title}`.

- [ ] **Step 1: Write the failing tests**

Append to `src/app/tests.rs`:

```rust
#[tokio::test]
async fn background_finish_lands_in_origin_session_and_notifies() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello");
    a.submit().unwrap();
    let origin = a.session.as_ref().unwrap().id.clone();

    a.new_session().unwrap(); // switch away mid-stream
    a.on_stream_event(crate::provider::StreamEvent::Token("late answer".into())).unwrap();
    a.on_stream_event(crate::provider::StreamEvent::Done).unwrap();

    // Landed in the origin session, not the (blank) active one.
    assert!(a.messages.is_empty());
    let stored = a.db.load_messages(&origin).unwrap();
    assert_eq!(stored.last().unwrap().role, "assistant");
    assert_eq!(stored.last().unwrap().content, "late answer");
    assert!(a.unread.contains(&origin));
    assert!(a.status.contains("response ready in"));
    assert!(a.stream_session.is_none());
}

#[tokio::test]
async fn background_tool_call_persists_to_origin_session_only() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello");
    a.submit().unwrap();
    let origin = a.session.as_ref().unwrap().id.clone();

    a.new_session().unwrap();
    a.on_stream_event(crate::provider::StreamEvent::ToolCall {
        name: "web_search".into(),
        arguments: "{}".into(),
        result: "ok".into(),
    })
    .unwrap();

    assert!(a.messages.is_empty()); // active transcript untouched
    let stored = a.db.load_messages(&origin).unwrap();
    assert_eq!(stored.last().unwrap().role, "tool_call");
}

#[tokio::test]
async fn send_while_streaming_names_the_busy_session() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello");
    a.submit().unwrap();
    a.new_session().unwrap();
    a.set_input("second message");
    a.submit().unwrap();
    assert!(a.status.contains("still streaming in"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test background_ send_while_streaming 2>&1 | tail -8` (run each: `cargo test background_finish_lands`, `cargo test background_tool_call`, `cargo test send_while_streaming`)
Expected: FAIL — response saved to active session / messages not empty / status says "wait for the current response".

- [ ] **Step 3: Implement**

In `src/app/chat.rs` `send_message`, replace the streaming guard:

```rust
        if self.is_streaming() {
            self.status = match &self.stream_session {
                Some((_, title)) => format!("wait — response still streaming in: {title}"),
                None => "wait for the current response to finish".to_string(),
            };
            self.set_input(&text);
            return Ok(());
        }
```

In `on_stream_event`, replace the ToolCall persistence + push block (keep the
`install_skill` reload line above it):

```rust
                let content =
                    serde_json::json!({ "name": name, "arguments": arguments, "result": result })
                        .to_string();
                // Persist to the stream's origin session (may not be active).
                let target = self
                    .stream_session
                    .as_ref()
                    .map(|(id, _)| id.clone())
                    .or_else(|| self.session.as_ref().map(|s| s.id.clone()));
                if let Some(id) = &target {
                    let _ = self.db.add_tool_call_message(id, &content);
                }
                if self.viewing_stream() {
                    self.messages.push(Message {
                        id: String::new(),
                        role: "tool_call".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        images: Vec::new(),
                    });
                }
```

Replace the Error arm:

```rust
            StreamEvent::Error(e) => {
                self.status = match (&self.stream_session, self.viewing_stream()) {
                    (Some((_, title)), false) => format!("stream error in {title}: {e}"),
                    _ => format!("stream error: {e}"),
                };
                self.finish_stream()?;
            }
```

Rewrite `finish_stream` (full function — routing, viewing-gated push/context,
away notification; the maybe_* trio reads active-session state so it only runs
when viewing):

```rust
    fn finish_stream(&mut self) -> Result<()> {
        self.stream_rx = None;
        self.stream_abort = None;
        self.tool_status = None;
        let origin = self.stream_session.take();
        let started = self.stream_started.take();
        let mut reasoning = std::mem::take(&mut self.thinking_text);
        let Some(buf) = self.streaming.take() else {
            return Ok(());
        };
        if buf.is_empty() {
            return Ok(());
        }
        // Some reasoning models (routed without the separate `reasoning` delta
        // field) inline their thinking as `<think>...</think>` in `content`
        // itself. Pull that out so the stored/displayed/copied message is just
        // the actual answer, not the thinking — same treatment as the explicit
        // reasoning channel above.
        let (buf, inline) = split_inline_reasoning(&buf);
        if let Some(inline) = inline {
            if !reasoning.is_empty() {
                reasoning.push('\n');
            }
            reasoning.push_str(&inline);
        }
        // Did the stream finish in the session the user is looking at?
        let viewing = match (&origin, &self.session) {
            (Some((id, _)), Some(s)) => *id == s.id,
            (None, _) => true,
            (Some(_), None) => false,
        };
        let model = self.current_model.clone();
        // Prefer the provider's exact usage; fall back to a ~4-chars/token estimate.
        let usage = self.stream_usage.take();
        let tokens = Some(match usage {
            Some(u) => u.completion_tokens as i64,
            None => buf.chars().count().div_ceil(4) as i64,
        });
        if viewing && let Some(u) = usage {
            // Some providers omit total; derive it from prompt + completion.
            let total = if u.total_tokens > 0 {
                u.total_tokens
            } else {
                u.prompt_tokens + u.completion_tokens
            };
            self.context_total = Some(total);
        }
        let secs = started.map(|s| s.elapsed().as_secs_f64());
        let reasoning = (!reasoning.is_empty()).then_some(reasoning);
        let phrase = Some(THINKING[self.thinking_idx].1.to_string());

        // The response always lands in its origin session.
        let target = origin
            .as_ref()
            .map(|(id, _)| id.clone())
            .or_else(|| self.session.as_ref().map(|s| s.id.clone()));
        if let Some(id) = &target {
            self.db.add_assistant_message(
                id,
                &buf,
                model.as_deref(),
                reasoning.as_deref(),
                tokens,
                secs,
                phrase.as_deref(),
            )?;
        }
        if viewing {
            self.messages.push(Message {
                id: String::new(),
                role: "assistant".to_string(),
                content: buf,
                model,
                reasoning,
                tokens,
                secs,
                phrase,
                images: Vec::new(),
            });
            // These read the *active* conversation, so they only make sense here.
            self.maybe_generate_title();
            self.maybe_extract_memory();
            self.maybe_compact();
        } else if let Some((id, title)) = origin {
            self.unread.insert(id);
            self.status = format!("✓ response ready in: {title}");
        }
        Ok(())
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test 2>&1 | tail -3`
Expected: all pass. (The existing `esc_stop_keeps_partial_response` test still passes: untagged/viewing streams behave exactly as before.)

- [ ] **Step 5: Commit**

```bash
git add src/app/chat.rs src/app/tests.rs
git commit -m "feat: stream results always land in their origin session, notify when away

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Session switch clears unread; deleting the streaming session discards the stream

**Files:**
- Modify: `src/app/sessions.rs` (`confirm_session` ~line 114, `confirm_delete` ~line 92)
- Modify: `src/app/chat.rs` (new `discard_stream`)
- Test: `src/app/tests.rs`

**Interfaces:**
- Consumes: `stream_session`, `unread` from Task 1.
- Produces: `App::discard_stream(&mut self)` — aborts and drops the stream without saving.

- [ ] **Step 1: Write the failing tests**

Append to `src/app/tests.rs`:

```rust
#[tokio::test]
async fn opening_a_session_clears_its_unread_marker() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello");
    a.submit().unwrap();
    let origin = a.session.as_ref().unwrap().id.clone();
    a.new_session().unwrap();
    a.on_stream_event(crate::provider::StreamEvent::Token("done".into())).unwrap();
    a.on_stream_event(crate::provider::StreamEvent::Done).unwrap();
    assert!(a.unread.contains(&origin));

    a.open_session_picker().unwrap();
    a.confirm_session().unwrap(); // most recent = origin
    assert_eq!(a.session.as_ref().unwrap().id, origin);
    assert!(!a.unread.contains(&origin));
    assert_eq!(a.messages.last().unwrap().content, "done"); // reloaded from db
}

#[tokio::test]
async fn deleting_the_streaming_session_discards_the_stream() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello");
    a.submit().unwrap();
    let origin = a.session.as_ref().unwrap().id.clone();
    a.on_stream_event(crate::provider::StreamEvent::Token("partial".into())).unwrap();

    a.open_session_picker().unwrap();
    a.confirm_delete().unwrap(); // deletes the only (streaming) session
    assert!(!a.is_streaming());
    assert!(a.stream_session.is_none());
    assert!(!a.unread.contains(&origin));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test opening_a_session_clears 2>&1 | tail -5` and `cargo test deleting_the_streaming_session 2>&1 | tail -5`
Expected: FAIL — unread still set / still streaming after delete (compile error for `discard_stream` first).

- [ ] **Step 3: Implement**

In `src/app/chat.rs`, next to `stop_stream`:

```rust
    /// Kill the in-flight stream and throw its partial text away — used when
    /// the origin session is deleted (nothing left to save into).
    pub(crate) fn discard_stream(&mut self) {
        if let Some(h) = self.stream_abort.take() {
            h.abort();
        }
        self.stream_rx = None;
        self.streaming = None;
        self.stream_session = None;
        self.thinking_text.clear();
        self.stream_usage = None;
        self.stream_started = None;
        self.tool_status = None;
    }
```

In `src/app/sessions.rs` `confirm_session`, after `self.messages = self.db.load_messages(&s.id)?;`:

```rust
            self.unread.remove(&s.id);
```

In `confirm_delete`, right after `self.db.delete_session(&s.id)?;`:

```rust
            if self.stream_session.as_ref().is_some_and(|(id, _)| *id == s.id) {
                self.discard_stream();
            }
            self.unread.remove(&s.id);
```

- [ ] **Step 4: Run tests**

Run: `cargo test 2>&1 | tail -3`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/app/chat.rs src/app/sessions.rs src/app/tests.rs
git commit -m "feat: clear unread on open; discard stream when its session is deleted

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: UI — render gates, hints, Esc scope, picker markers

**Files:**
- Modify: `src/ui/history.rs` (streaming-tail gate, ~line 42 `if app.streaming.is_some()`)
- Modify: `src/app/mod.rs` (`is_welcome`, ~line 765)
- Modify: `src/ui/mod.rs` (`render_input` hint, ~line 70)
- Modify: `src/events.rs` (Esc arm, ~line 287)
- Modify: `src/ui/popups/session.rs` (list item markers, ~line 24)
- Test: `src/app/tests.rs`

**Interfaces:**
- Consumes: `viewing_stream()`, `stream_session`, `unread`.
- Produces: nothing new — behavior only.

- [ ] **Step 1: Write the failing test**

UI paints aren't unit-tested here; the testable seam is the welcome/tail gate. Append to `src/app/tests.rs`:

```rust
#[tokio::test]
async fn welcome_screen_shows_while_a_stream_runs_elsewhere() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    a.set_input("hello");
    a.submit().unwrap();
    assert!(!a.is_welcome()); // viewing the streaming session
    a.new_session().unwrap();
    assert!(a.is_welcome()); // blank chat, stream backgrounded
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test welcome_screen_shows_while 2>&1 | tail -5`
Expected: FAIL — `is_welcome()` returns false because `streaming.is_some()`.

- [ ] **Step 3: Implement**

`src/app/mod.rs` — replace `is_welcome`:

```rust
    /// The empty start screen (banner + greeting + clock) shows when there's no
    /// conversation yet — a stream running in another session doesn't hide it.
    pub fn is_welcome(&self) -> bool {
        self.messages.is_empty() && !self.viewing_stream()
    }
```

`src/ui/history.rs` — in `render_history`, change the tail condition:

```rust
    if app.viewing_stream() {
        push_assistant_streaming(&mut tail, app, width, &mut tail_code, &mut tail_blocks);
        tail_code.resize(tail.len(), None);
    }
```

`src/ui/mod.rs` — replace the hint computation in `render_input` (becomes a
`String`; `Line::from(hint)` accepts it unchanged):

```rust
    let hint = if app.settings.hide_hints {
        String::new()
    } else if app.viewing_stream() {
        " …working (Esc to stop) ".to_string()
    } else if let Some((_, title)) = app.stream_session.as_ref().filter(|_| app.is_streaming()) {
        format!(" ⟳ streaming in: {title} ")
    } else {
        " message (Enter to send, /help) ".to_string()
    };
```

`src/events.rs` — Esc stops only the stream you're looking at:

```rust
        // Esc while viewing the streaming session stops the response (partial
        // text is kept); otherwise it clears the composer.
        KeyCode::Esc if app.viewing_stream() => app.stop_stream()?,
```

`src/ui/popups/session.rs` — in the list-item closure, add a marker before the
`#id` span and account for its width in the gap:

```rust
            let streaming_here =
                app.stream_session.as_ref().is_some_and(|(id, _)| *id == s.id);
            let marker = if streaming_here {
                Some(Span::styled("⟳ ", Style::default().fg(Color::Cyan)))
            } else if app.unread.contains(&s.id) {
                Some(Span::styled("● ", Style::default().fg(Color::Yellow)))
            } else {
                None
            };
            let mlen = if marker.is_some() { 2 } else { 0 };
            let gap =
                width.saturating_sub(mlen + id.chars().count() + 1 + when.chars().count() + 2);
            let mut top_spans = Vec::new();
            if let Some(m) = marker {
                top_spans.push(m);
            }
            top_spans.extend([
                Span::styled(format!("#{id}"), Style::default().fg(Color::Cyan)),
                Span::raw(" ".repeat(gap)),
                Span::styled(when, dim),
            ]);
            let top = Line::from(top_spans);
```

(This replaces the existing `let gap = …;` and `let top = Line::from(vec![…]);` block.)

- [ ] **Step 4: Run tests**

Run: `cargo test 2>&1 | tail -3`
Expected: all pass. Also `cargo build 2>&1 | tail -3` — zero warnings.

- [ ] **Step 5: Manual smoke test**

Run the app: send a slow prompt, `/new` mid-stream → welcome screen + `⟳ streaming in:` hint; `/session` shows `⟳` on the busy session; wait → status `✓ response ready in:` + `●` marker; open it → full response present, marker gone; Esc in the other session only clears the composer.

- [ ] **Step 6: Commit**

```bash
git add src/ui/history.rs src/ui/mod.rs src/app/mod.rs src/events.rs src/ui/popups/session.rs src/app/tests.rs
git commit -m "feat: background-stream UI — render gates, session markers, scoped Esc

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
