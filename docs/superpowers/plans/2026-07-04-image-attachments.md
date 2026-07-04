# Image Attachments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pasted images attach to the message: vision models receive the actual image (resent with history every turn), non-vision models receive a rich, silently-injected description generated once by the image model. The transcribe-into-composer flow is removed.

**Architecture:** `Model.supports_images` comes from OpenRouter's `architecture.input_modalities`. Pasted images are saved as PNGs under `spaces/<name>/images/` and recorded in a new `message_images` table keyed by the message's db id (now plumbed into the in-memory `Message`). `ChatMessage` gains `images: Vec<String>` (data URLs) with a manual `Serialize` that emits a plain string `content` (wire unchanged) or an OpenAI parts array. `send_message` gains a pre-send gate: non-vision model + undescribed images → deferred send while the image model fills descriptions (persisted once, reused forever).

**Tech Stack:** Rust; existing deps only (`png`, `base64`, `arboard`, `serde`, `rusqlite`).

**Spec:** `docs/superpowers/specs/2026-07-04-image-attachments-design.md`

## Global Constraints

- Minimum visibility that compiles per item; grep call sites first.
- `cargo build` warning-free after every task (temporary `#[allow(dead_code)] // used from Task N of the image plan; remove with first caller` sanctioned for items whose callers land later — remove them in the consuming task).
- `cargo test` fully passing after every task.
- Stage only exact touched files; never `git add -A`.
- Wire format for image-free messages must be byte-identical to today (plain string `content`).
- Setting key stays `transcriber_model` (db compat); only the UI label changes.
- The main chat model NEVER receives image data unless `supports_images`.
- Descriptions are generated at most once per image and persisted.

---

### Task 1: `Model.supports_images` from input_modalities

**Files:**
- Modify: `src/provider/mod.rs` (Model field)
- Modify: `src/provider/openrouter.rs` (parse)

**Interfaces:**
- Produces: `Model.supports_images: bool` (used by Tasks 5, 6).

- [ ] **Step 1: Failing test** (in openrouter.rs tests):

```rust
#[test]
fn parses_input_modalities_into_supports_images() {
    let json = r#"{"data":[
        {"id":"a/vision","architecture":{"input_modalities":["text","image"]}},
        {"id":"b/text","architecture":{"input_modalities":["text"]}},
        {"id":"c/legacy"}
    ]}"#;
    let resp: ModelsResponse = serde_json::from_str(json).unwrap();
    let flags: Vec<bool> = resp.data.iter().map(entry_supports_images).collect();
    assert_eq!(flags, vec![true, false, false]);
}
```

- [ ] **Step 2: Run** `cargo test parses_input_modalities` → compile FAIL.

- [ ] **Step 3: Implement.** In openrouter.rs:

```rust
#[derive(Deserialize)]
struct ModelEntry {
    // …existing fields…
    #[serde(default)]
    architecture: Option<Architecture>,
}

#[derive(Deserialize)]
struct Architecture {
    #[serde(default)]
    input_modalities: Vec<String>,
}

/// Whether the catalog entry accepts image input.
fn entry_supports_images(e: &ModelEntry) -> bool {
    e.architecture
        .as_ref()
        .is_some_and(|a| a.input_modalities.iter().any(|m| m == "image"))
}
```

In `list_models`'s map: `supports_images: entry_supports_images(&m),` — note the closure currently consumes `m` by value; compute the flag before moving fields, or take `&m` first.

In `src/provider/mod.rs`, `Model` gains:

```rust
/// Whether the model accepts image input (`architecture.input_modalities`).
pub supports_images: bool,
```

Update every `Model { … }` struct literal (grep `Model {` — there are constructions in app tests/helpers) with `supports_images: false` except where a test needs true.

- [ ] **Step 4:** `cargo test` all pass; `cargo build` zero warnings.
- [ ] **Step 5: Commit** — `git add src/provider/mod.rs src/provider/openrouter.rs` (+ any test files touched for struct literals) → `feat: detect image-capable models from input_modalities`

---

### Task 2: Message id + `message_images` table

**Files:**
- Modify: `src/db.rs`
- Modify: `src/app/chat.rs`, `src/app/tests.rs` and any other `Message { … }` literal sites (grep) — mechanical field additions only.

**Interfaces:**
- Produces (Tasks 5, 6 consume):
  - `Message.id: String`, `Message.images: Vec<MessageImage>`
  - `pub struct MessageImage { pub id: String, pub path: String, pub description: Option<String> }`
  - `Db::add_user_message(&self, session_id, content) -> Result<String>` (now returns the message id)
  - `Db::add_message_images(&self, message_id: &str, paths: &[String]) -> Result<Vec<MessageImage>>`
  - `Db::set_image_description(&self, image_id: &str, description: &str) -> Result<()>`
  - `Db::load_messages` fills `images` (empty for most messages)

- [ ] **Step 1: Failing test** (db.rs tests):

```rust
#[test]
fn message_images_roundtrip_and_description_backfill() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space).unwrap();
    let mid = db.add_user_message(&s.id, "look at this").unwrap();
    let imgs = db.add_message_images(&mid, &["/tmp/a.png".into(), "/tmp/b.png".into()]).unwrap();
    assert_eq!(imgs.len(), 2);
    assert!(imgs[0].description.is_none());

    db.set_image_description(&imgs[0].id, "a red square").unwrap();
    let msgs = db.load_messages(&s.id).unwrap();
    assert_eq!(msgs[0].id, mid);
    assert_eq!(msgs[0].images.len(), 2);
    assert_eq!(msgs[0].images[0].description.as_deref(), Some("a red square"));
    assert!(msgs[0].images[1].description.is_none());
    // Non-image messages stay empty.
    db.add_user_message(&s.id, "plain").unwrap();
    let msgs = db.load_messages(&s.id).unwrap();
    assert!(msgs[1].images.is_empty());
}
```

- [ ] **Step 2:** compile FAIL.

- [ ] **Step 3: Implement.**

Schema (append to `migrate`'s batch):

```sql
CREATE TABLE IF NOT EXISTS message_images (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    path TEXT NOT NULL,
    description TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_message_images_message ON message_images(message_id);
```

Struct + methods:

```rust
/// An image attached to a message: PNG on disk plus the one-time generated
/// description used when the active model can't see images.
#[derive(Debug, Clone)]
pub struct MessageImage {
    pub id: String,
    pub path: String,
    pub description: Option<String>,
}
```

- `Message` gains `pub id: String` and `pub images: Vec<MessageImage>`.
- `insert_message` returns `Result<String>` (the generated uuid).
- `add_user_message` returns that id; `add_assistant_message` keeps returning `Ok(())` (callers don't need it — `let _ = self.insert_message(...)?; Ok(())` style, or return the id there too if it's less code; pick the smaller diff).
- `add_message_images`: one INSERT per path with fresh uuid + now; return the built `Vec<MessageImage>` (descriptions None).
- `set_image_description`: `UPDATE message_images SET description = ?2 WHERE id = ?1`.
- `load_messages`: select `id` as an extra column; after collecting messages, one query `SELECT message_id, id, path, description FROM message_images WHERE message_id IN (…)` is awkward in rusqlite — simpler: `SELECT m.id, mi.id, mi.path, mi.description FROM message_images mi JOIN messages m ON m.id = mi.message_id WHERE m.session_id = ?1 ORDER BY mi.created_at ASC`, build a `HashMap<String, Vec<MessageImage>>`, then attach by message id.
- `delete_session` also deletes its message_images:
  `DELETE FROM message_images WHERE message_id IN (SELECT id FROM messages WHERE session_id = ?1)` (before deleting messages).

Non-db `Message { … }` literal sites (chat.rs pushes, tests): add `id: String::new(), images: Vec::new()` — EXCEPT the user-message push in `send_message`, which should use the id returned by `add_user_message` (and Task 5 fills images; leave `images: Vec::new()` for now). If dead_code warns on anything (e.g. `MessageImage.id` before Task 5 reads it), use the sanctioned temporary allow.

- [ ] **Step 4:** `cargo test` all pass; zero warnings.
- [ ] **Step 5: Commit** → `feat: message ids + message_images table with description backfill`

---

### Task 3: ChatMessage image parts + richer image-model prompt

**Files:**
- Modify: `src/provider/mod.rs` (manual Serialize for ChatMessage, `images` field)
- Modify: `src/provider/openrouter.rs` (rename `transcribe_image` → `describe_image`; new prompt in `vision_body`)
- Modify: `src/app/transcribe.rs` (caller rename only — full rewrite comes in Task 4)

**Interfaces:**
- Produces: `ChatMessage.images: Vec<String>` (data URLs; empty = plain string wire format); `OpenRouter::describe_image(model, data_url)`.

- [ ] **Step 1: Failing tests** (provider/mod.rs or openrouter.rs tests):

```rust
#[test]
fn chat_message_serializes_string_content_when_no_images() {
    let m = ChatMessage::text("user", "hi");
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["content"], "hi");
    assert!(v.get("tool_calls").is_none());
}

#[test]
fn chat_message_serializes_parts_when_images_present() {
    let mut m = ChatMessage::text("user", "what is this?");
    m.images = vec!["data:image/png;base64,AAAA".into()];
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["content"][0]["type"], "text");
    assert_eq!(v["content"][0]["text"], "what is this?");
    assert_eq!(v["content"][1]["type"], "image_url");
    assert_eq!(v["content"][1]["image_url"]["url"], "data:image/png;base64,AAAA");
}

#[test]
fn chat_message_with_tool_calls_still_serializes_them() {
    let m = ChatMessage {
        role: "assistant".into(),
        content: "".into(),
        tool_calls: Some(vec![ToolCall { id: "c1".into(), name: "web_search".into(), arguments: "{}".into() }]),
        tool_call_id: None,
        images: Vec::new(),
    };
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["tool_calls"][0]["function"]["name"], "web_search");
    assert_eq!(v["tool_calls"][0]["type"], "function");
}
```

- [ ] **Step 2:** compile FAIL.

- [ ] **Step 3: Implement.** In provider/mod.rs: add `pub images: Vec<String>` to ChatMessage (keep `Default`), drop `derive(Serialize)` + the `serialize_with` attribute, and write a manual impl reusing the existing `Wire`/`Function` shapes:

```rust
impl Serialize for ChatMessage {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(None)?;
        map.serialize_entry("role", &self.role)?;
        if self.images.is_empty() {
            map.serialize_entry("content", &self.content)?;
        } else {
            // OpenAI vision shape: text part (if any) + one image_url part per image.
            let mut parts: Vec<serde_json::Value> = Vec::new();
            if !self.content.is_empty() {
                parts.push(serde_json::json!({ "type": "text", "text": self.content }));
            }
            for url in &self.images {
                parts.push(serde_json::json!({ "type": "image_url", "image_url": { "url": url } }));
            }
            map.serialize_entry("content", &parts)?;
        }
        if let Some(calls) = &self.tool_calls {
            let wire: Vec<Wire> = calls
                .iter()
                .map(|c| Wire { id: &c.id, r#type: "function", function: Function { name: &c.name, arguments: &c.arguments } })
                .collect();
            map.serialize_entry("tool_calls", &wire)?;
        }
        if let Some(id) = &self.tool_call_id {
            map.serialize_entry("tool_call_id", id)?;
        }
        map.end()
    }
}
```

(Move `Wire`/`Function` out of the old `serialize_tool_calls` fn — delete that fn — and derive `Serialize` on them at module level.)

In openrouter.rs: rename `transcribe_image` → `describe_image` (update the one caller in src/app/transcribe.rs mechanically; deeper rewrite is Task 4) and replace `vision_body`'s instruction text with:

```
"Describe this image so another AI model can reason about it without seeing it. \
 Cover: what it is (screenshot, chart, photo, diagram…), overall layout and structure, \
 the key entities and how they relate, ALL visible text verbatim (preserve code, \
 tables, and labels as markdown), and any notable visual details (colors, states, \
 highlights, errors). Be thorough but do not speculate beyond what is visible."
```

Update the existing `vision_body_has_image_url_content_part` test's "transcribe" substring assertion to match (e.g. assert it contains "Describe this image").

- [ ] **Step 4:** `cargo test` all pass (incl. existing request-shape tests untouched); zero warnings.
- [ ] **Step 5: Commit** → `feat: multimodal ChatMessage serialization + understanding-oriented image prompt`

---

### Task 4: Attachment state, paste, and describe machinery

**Files:**
- Modify: `src/space.rs` (`images_dir`)
- Rewrite: `src/app/transcribe.rs` → keep filename, new content (attachments + describe)
- Modify: `src/app/mod.rs` (fields, AppEvent, next_event)
- Modify: `src/input.rs` (paste_from_clipboard branch)
- Modify: `src/events.rs` (event arm; Esc clears attachments)

**Interfaces:**
- Produces (Task 5 consumes):
  - `pub struct PendingImage { pub path: std::path::PathBuf, pub data_url: String }` (in transcribe.rs)
  - App fields: `pub pending_images: Vec<PendingImage>`, `pub(crate) describe_rx: Option<mpsc::UnboundedReceiver<(String, std::result::Result<String, String>)>>`, `pub(crate) deferred_send: Option<String>`
  - `App::attach_clipboard_image(&mut self, img: arboard::ImageData)` — encode, save PNG to `spaces/<name>/images/<uuid>.png`, push PendingImage, status `image attached (Esc clears)`
  - `App::start_describing(&mut self, todo: Vec<(String, String)>)` — (image_id, path) pairs; spawns ONE task that describes sequentially, sending `(image_id, result)` per image
  - `App::on_described(&mut self, r: Option<(String, std::result::Result<String, String>)>)` — persist + update in-memory; on `None` (channel closed) hand control to `App::resume_deferred_send()` (Task 5 implements the resume; in THIS task it is a stub that just clears `deferred_send` — mark with the sanctioned temporary allow note if needed)
  - `AppEvent::Described(Option<(String, std::result::Result<String, String>)>)` replaces `AppEvent::Transcript`
  - `Space::images_dir(&self, name: &str) -> PathBuf`
- Removed: `transcribe_clipboard_image`, `on_transcript_result`, `transcript_rx`, `AppEvent::Transcript` (and their events.rs arm / next_event arm / tests).

- [ ] **Step 1: Failing tests** (transcribe.rs tests; keep `encodes_rgba_as_png_data_url` as is):

```rust
#[test]
fn attach_saves_png_and_pushes_pending() {
    let mut a = crate::app::tests::app_with_key();
    let img = arboard::ImageData {
        width: 2, height: 1,
        bytes: std::borrow::Cow::Owned(vec![255, 0, 0, 255, 0, 0, 0, 0]),
    };
    a.attach_clipboard_image(img);
    assert_eq!(a.pending_images.len(), 1);
    assert!(a.pending_images[0].path.exists());
    assert!(a.pending_images[0].data_url.starts_with("data:image/png;base64,"));
    assert!(a.status.contains("image attached"));
}

#[test]
fn described_result_persists_description() {
    let mut a = crate::app::tests::app_with_key();
    // Seed one message with an image via the db layer.
    let s = a.db.create_session("t", "a/b", &a.active_space.id).unwrap();
    let mid = a.db.add_user_message(&s.id, "see").unwrap();
    let imgs = a.db.add_message_images(&mid, &["/tmp/x.png".into()]).unwrap();
    a.session = Some(s.clone());
    a.messages = a.db.load_messages(&s.id).unwrap();

    a.on_described(Some((imgs[0].id.clone(), Ok("a diagram of the login flow".into()))));
    assert_eq!(
        a.messages[0].images[0].description.as_deref(),
        Some("a diagram of the login flow")
    );
    let reloaded = a.db.load_messages(&s.id).unwrap();
    assert_eq!(reloaded[0].images[0].description.as_deref(), Some("a diagram of the login flow"));
}
```

- [ ] **Step 2:** compile FAIL.

- [ ] **Step 3: Implement.**

`src/space.rs`:

```rust
/// Directory holding a space's pasted conversation images.
pub fn images_dir(&self, name: &str) -> PathBuf {
    self.space_dir(name).join("images")
}
```

`src/app/transcribe.rs` (module doc becomes "Conversation image attachments…"; `png_data_url` stays):

```rust
/// A pasted image waiting to be sent with the next message.
pub struct PendingImage {
    pub path: std::path::PathBuf,
    pub data_url: String,
}

impl App {
    /// Save a clipboard image to the space's images dir and stage it for the
    /// next message. No model call happens here — vision models get the raw
    /// image at send; non-vision models trigger description on demand.
    pub fn attach_clipboard_image(&mut self, img: arboard::ImageData) {
        let data_url = match png_data_url(img.width, img.height, &img.bytes) {
            Ok(u) => u,
            Err(e) => {
                self.status = format!("could not encode image: {e}");
                return;
            }
        };
        let dir = self.space.images_dir(&self.active_space.name);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.status = format!("could not create {}: {e}", dir.display());
            return;
        }
        let path = dir.join(format!("{}.png", uuid::Uuid::new_v4()));
        // data_url is "data:image/png;base64,<payload>" — decode the payload back
        // is wasteful; re-encode instead: write the PNG bytes we just built.
        // (Restructure png_data_url usage: build bytes once, then both write
        // them to disk and base64 them. See below.)
        …
        self.pending_images.push(PendingImage { path, data_url });
        let n = self.pending_images.len();
        self.status = format!("{n} image{} attached (Esc clears)", if n == 1 { "" } else { "s" });
    }
}
```

To avoid encode-decode churn, split the helper:

```rust
/// Encode raw RGBA pixels as PNG bytes.
pub(super) fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>> { …existing body, return bytes… }

/// `data:image/png;base64,…` URL for PNG bytes.
pub(super) fn png_bytes_data_url(bytes: &[u8]) -> String {
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}
```

(`png_data_url(w,h,rgba)` remains as a thin composition of the two so the existing test keeps passing.) `attach_clipboard_image` calls `encode_png`, writes bytes with `std::fs::write(&path, &bytes)` (status on error), then `png_bytes_data_url(&bytes)`.

Describe machinery:

```rust
/// Describe `todo` images ((message_images row id, png path)) with the image
/// model, one at a time; results arrive as AppEvent::Described.
pub(crate) fn start_describing(&mut self, todo: Vec<(String, String)>) {
    let Some(provider) = self.provider.clone() else { return };
    let model = self.transcriber_model.trim().to_string();
    let (tx, rx) = mpsc::unbounded_channel();
    self.describe_rx = Some(rx);
    self.status = "understanding image…".to_string();
    tokio::spawn(async move {
        for (image_id, path) in todo {
            let result = match std::fs::read(&path) {
                Ok(bytes) => {
                    let url = png_bytes_data_url(&bytes);
                    provider.describe_image(&model, &url).await.map_err(|e| e.to_string())
                }
                Err(e) => Err(format!("could not read {path}: {e}")),
            };
            let _ = tx.send((image_id, result));
        }
    });
}

/// One description finished (or the channel closed → all done).
pub fn on_described(&mut self, r: Option<(String, std::result::Result<String, String>)>) {
    match r {
        Some((image_id, Ok(desc))) => {
            let _ = self.db.set_image_description(&image_id, &desc);
            for m in &mut self.messages {
                for img in &mut m.images {
                    if img.id == image_id {
                        img.description = Some(desc.clone());
                    }
                }
            }
        }
        Some((_, Err(e))) => {
            self.status = format!("image understanding failed: {e}");
            self.deferred_send = None; // abort the pending send; user retries
        }
        None => {
            self.describe_rx = None;
            self.resume_deferred_send();
        }
    }
}
```

`src/app/mod.rs`: replace `transcript_rx` with `describe_rx` + add `pending_images: Vec::new()`, `deferred_send: None`; `AppEvent::Transcript` → `Described(Option<(String, std::result::Result<String, String>)>)`; next_event arm updated. Add a stub

```rust
/// Continue a send that waited on image descriptions (filled in by the send
/// flow; nothing to do until then).
pub(crate) fn resume_deferred_send(&mut self) {
    self.deferred_send = None;
}
```

in chat.rs or transcribe.rs (Task 5 replaces the body).

`src/input.rs` `paste_from_clipboard`: `self.transcribe_clipboard_image(img)` → `self.attach_clipboard_image(img)`.

`src/events.rs`: `AppEvent::Described(r) => app.on_described(r),`. In `handle_normal`'s `KeyCode::Esc` arm: also `app.pending_images.clear();` (composer Esc clears text and attachments together).

Delete `transcribe_clipboard_image`, `on_transcript_result`, and the `transcript_result_lands_in_composer` test.

- [ ] **Step 4:** `cargo test` all pass; zero warnings (temporary allows where Task 5 is the first caller — likely `start_describing`, `deferred_send`, maybe `PendingImage.data_url`).
- [ ] **Step 5: Commit** → `feat: image attachments staged at paste + background describe machinery`

---

### Task 5: Send flow — attach, multimodal history, deferred send

**Files:**
- Modify: `src/app/chat.rs` (send_message, new `build_history`, `resume_deferred_send` real body)
- Modify: `src/app/models.rs` (tiny helper `current_model_supports_images`)
- Modify: `src/app/tests.rs` (history-assembly tests)

**Interfaces:**
- Consumes: everything from Tasks 1-4.
- Produces: complete feature behavior.

- [ ] **Step 1: Failing tests** (app/tests.rs):

```rust
#[test]
fn history_carries_image_parts_for_vision_models_and_text_for_others() {
    let mut a = app_with_key();
    let s = a.db.create_session("t", "vis/model", &a.active_space.id).unwrap();
    let mid = a.db.add_user_message(&s.id, "what is this?").unwrap();
    // A real tiny png on disk so the vision path can read it back.
    let dir = a.space.images_dir(&a.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    let png_path = dir.join("t.png");
    std::fs::write(&png_path, crate::app::transcribe::encode_png(1, 1, &[0, 0, 0, 255]).unwrap()).unwrap();
    let imgs = a.db.add_message_images(&mid, &[png_path.to_string_lossy().to_string()]).unwrap();
    a.db.set_image_description(&imgs[0].id, "a black pixel").unwrap();
    a.session = Some(s.clone());
    a.messages = a.db.load_messages(&s.id).unwrap();
    a.models = vec![
        Model { id: "vis/model".into(), name: "v".into(), supports_reasoning: false, context_length: None, supports_images: true },
        Model { id: "txt/model".into(), name: "t".into(), supports_reasoning: false, context_length: None, supports_images: false },
    ];

    a.current_model = Some("vis/model".into());
    let h = a.build_history();
    let user = h.iter().find(|m| m.role == "user").unwrap();
    assert_eq!(user.images.len(), 1);
    assert!(user.images[0].starts_with("data:image/png;base64,"));
    assert_eq!(user.content, "what is this?");

    a.current_model = Some("txt/model".into());
    let h = a.build_history();
    let user = h.iter().find(|m| m.role == "user").unwrap();
    assert!(user.images.is_empty());
    assert!(user.content.contains("[Image: a black pixel]"));
}

#[test]
fn missing_descriptions_are_collected_for_non_vision_sends() {
    let mut a = app_with_key();
    let s = a.db.create_session("t", "txt/model", &a.active_space.id).unwrap();
    let mid = a.db.add_user_message(&s.id, "see").unwrap();
    a.db.add_message_images(&mid, &["/tmp/nope.png".into()]).unwrap();
    a.session = Some(s.clone());
    a.messages = a.db.load_messages(&s.id).unwrap();
    let missing = a.undescribed_images();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].1, "/tmp/nope.png");
}
```

(Adjust `Model` literal fields to the real struct. `encode_png` needs `pub(crate)` reachability from tests.rs — tests.rs is inside `app`, `pub(super)` suffices; verify.)

- [ ] **Step 2:** compile FAIL.

- [ ] **Step 3: Implement** in chat.rs:

Helper in models.rs:

```rust
/// Whether the active model accepts image input (unknown model → false).
pub(crate) fn current_model_supports_images(&self) -> bool {
    self.current_model
        .as_deref()
        .is_some_and(|id| self.models.iter().any(|m| m.id == id && m.supports_images))
}
```

In chat.rs — extract history assembly from `send_message` into:

```rust
/// The exact message list a completion request will carry: system prompt,
/// compaction digest, forced skill, then the effective conversation tail.
/// User messages with images become multimodal parts for vision models, or
/// get their stored descriptions appended as text for everything else.
pub(crate) fn build_history(&mut self) -> Vec<ChatMessage> {
    // …existing system/compaction/forced-skill blocks move here unchanged
    // (forced_skill.take() stays — call build_history once per send)…
    let vision = self.current_model_supports_images();
    for m in self.effective_messages() {
        let mut cm = ChatMessage::text(m.role.clone(), m.content.clone());
        if m.role == "user" && !m.images.is_empty() {
            if vision {
                // ponytail: PNGs re-read from disk every send; cache in RAM if
                // large sessions ever make this noticeable.
                cm.images = m
                    .images
                    .iter()
                    .filter_map(|img| std::fs::read(&img.path).ok())
                    .map(|bytes| crate::app::transcribe::png_bytes_data_url(&bytes))
                    .collect();
            } else {
                for img in &m.images {
                    if let Some(d) = &img.description {
                        cm.content.push_str(&format!("\n\n[Image: {d}]"));
                    }
                }
            }
        }
        history.push(cm);
    }
    history
}
```

(Note `effective_messages` borrows `self` while we also need `&mut` for `forced_skill.take()` — order the blocks so the take happens before the loop, exactly as `send_message` does today; cloning the effective slice is acceptable if the borrow checker demands it — messages are cloned today anyway.)

```rust
/// (message_images row id, path) for every image in the effective range
/// that still has no description.
pub(crate) fn undescribed_images(&self) -> Vec<(String, String)> {
    self.effective_messages()
        .iter()
        .flat_map(|m| m.images.iter())
        .filter(|i| i.description.is_none())
        .map(|i| (i.id.clone(), i.path.clone()))
        .collect()
}
```

`send_message` changes (order matters):

1. Existing guards (streaming, provider, model) unchanged.
2. NEW pre-attach: after the session exists and BEFORE `add_user_message` — no: images must attach to THIS message, so first insert the user message (`let mid = self.db.add_user_message(&session_id, &text)?;`), then if `!self.pending_images.is_empty()`:
   ```rust
   let paths: Vec<String> = self.pending_images.iter().map(|p| p.path.to_string_lossy().to_string()).collect();
   let images = self.db.add_message_images(&mid, &paths)?;
   self.pending_images.clear();
   // in-memory push carries them:
   self.messages.push(Message { id: mid, role: "user".into(), content: text, images, …None fields… });
   ```
   (plain path pushes `images: Vec::new()`).
3. NEW deferred gate, after the push, before building history:
   ```rust
   if !self.current_model_supports_images() {
       let missing = self.undescribed_images();
       if !missing.is_empty() {
           if self.transcriber_model.trim().is_empty() {
               self.status = "model can't see images — set an image model in /config or pick a vision model".to_string();
               return Ok(());
           }
           self.deferred_send = Some(String::new()); // marker: resume streams the reply
           self.start_describing(missing);
           return Ok(());
       }
   }
   ```
   Note the user message is already stored/pushed, so the "deferred text" is empty — resuming only needs to fire the request. Rework the tail of `send_message` into a private `fn start_stream(&mut self) -> Result<()>` (builds history via `build_history`, params, tools, spawns `stream_chat`, sets spinner state) so both `send_message` and the resume share it.
4. Replace the Task-4 stub:
   ```rust
   /// All descriptions arrived: fire the request that was waiting on them.
   pub(crate) fn resume_deferred_send(&mut self) {
       if self.deferred_send.take().is_some() {
           let _ = self.start_stream();
       }
   }
   ```
   (`on_described`'s error arm already clears `deferred_send`, so a failed description aborts cleanly with a status message.)

Remove any Task-4/Task-1 temporary allows now consumed (`start_describing`, `deferred_send`, `PendingImage.data_url` if the vision send path uses it — note: build_history re-reads PNGs from disk, so `data_url` on PendingImage may end up unread → if so, DELETE the field rather than keeping it dead).

- [ ] **Step 4:** `cargo test` all pass; zero warnings.
- [ ] **Step 5: Commit** → `feat: image-aware send flow with deferred description for non-vision models`

---

### Task 6: UI surfaces

**Files:**
- Modify: `src/ui/mod.rs` (`render_input` attachment indicator)
- Modify: `src/ui/history.rs` (image marker on user messages)
- Modify: `src/ui/popups/model.rs` (vision glyph in the picker rows)
- Modify: `src/app/mod.rs` (settings label rename)

- [ ] **Step 1: Implement (small, mechanical):**

1. `render_input`: when `!app.pending_images.is_empty()`, add a third title to the block:
   ```rust
   .title_top(Line::from(Span::styled(
       format!(" 📎 {} image{} — Esc clears ", n, if n == 1 { "" } else { "s" }),
       Style::default().fg(Color::Yellow),
   )))
   ```
   (bind `n = app.pending_images.len()` first; place between the hint and the session-name title).
2. `history.rs`: `push_user` currently takes `&m.content` — change the call site to pass the message and, when `!m.images.is_empty()`, push a dim marker line first:
   ```rust
   out.push(Line::from(dim(format!("🖼 {} image{}", m.images.len(), if m.images.len() == 1 { "" } else { "s" }))));
   ```
   (smallest diff: do it at the call site in `wrap_conversation` before `push_user`, leaving `push_user` untouched).
3. Model picker rows: find where each model line is built in `src/ui/popups/model.rs` (`model_items`) and append a dim `⊡` span when `m.supports_images` (mirror however `supports_reasoning` is indicated; if reasoning has no indicator, add just the image glyph, dim, after the name).
4. `SettingsField::TranscriberModel::label()` → `"image model (Enter to pick, Backspace clears)"`. Also update the two status strings in models.rs (`"transcriber model: {id}"` → `"image model: {id}"`, `"transcriber model cleared — image paste disabled"` → `"image model cleared — image descriptions disabled"`) and the Task-10 test if it asserts on labels.

- [ ] **Step 2:** `cargo build` zero warnings; `cargo test` all pass (fix any label-asserting tests).
- [ ] **Step 3: Commit** → `feat: attachment chip, history image markers, vision glyph, image-model label`

---

### Task 7: Verification pass

- [ ] `cargo build` (zero warnings) + `cargo test` (report counts).
- [ ] Greps: no `transcribe_clipboard_image`/`on_transcript_result`/`AppEvent::Transcript` remain; no `used from Task N of the image plan` allows remain; `describe_image` has exactly the two callers (transcribe.rs describe loop; none elsewhere).
- [ ] Confirm wire compat: run the ChatMessage serialization tests; grep that `ChatMessage::text` callers all leave `images` empty.
- [ ] Manual smoke (human, API key): paste screenshot → chip; send with a vision model (`⊡` in picker) → model describes what it sees; ask a follow-up → it can still see it; switch to a non-vision model → next send shows "understanding image…", then answers from the description; reopen the session → images survive.
