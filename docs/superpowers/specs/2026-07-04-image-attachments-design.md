# Image Attachments — Design

Date: 2026-07-04
Status: approved (supersedes the transcribe-into-composer flow from
2026-07-03-space-filesets-design.md §Image paste)

## Problem

The v1 image flow (transcribe clipboard image → text into composer) flattens
images into OCR text. The model never *sees* anything — layout, charts,
UI screenshots, diagrams all reduce to words. Vision-capable models should get
the actual image; non-vision models should get a rich understanding of it, not
a bare transcription.

## Decisions (user-approved)

| Question | Decision |
|---|---|
| Vision-capable model | Image attaches to the user message as an `image_url` content part (base64 data URL) and is **resent with history on every subsequent turn** while the session lives (Open WebUI/LibreChat behavior); compaction eventually folds it away |
| Non-vision model | The image model generates a **rich description** (scene, layout, entities and relationships, all text verbatim) injected **silently at send** — the user never sees or edits it |
| Old flow | Transcribe-into-composer is removed |

## Architecture

### Capability detection
- `Model` gains `supports_images: bool`, parsed from the OpenRouter `/models`
  response: `architecture.input_modalities` contains `"image"`.

### Paste → attachment
- Ctrl+V with a clipboard image (popup == None): PNG is encoded (existing
  `png_data_url` machinery) and **saved to disk** at
  `spaces/<name>/images/<uuid>.png`; an entry is pushed to
  `App::pending_images: Vec<PendingImage { path, data_url }>`.
- The composer's border title shows an attachment indicator
  (`📎 1 image — Esc clears`); nothing is inserted into the composer text.
- Esc (which already clears the composer) also clears pending attachments.
- Multiple images per message allowed.

### Persistence
- In-memory `db::Message` gains its db `id` (plumbed through
  `load_messages`/inserts) and `images: Vec<MessageImage>`.
- New table `message_images (id, message_id, path, description, created_at)`;
  `description` is NULL until first generated, then persisted (generated at
  most once per image).

### Send flow
- On send with pending images, the images are recorded against the new user
  message row.
- Building history (every turn): for each user message with images —
  - active model `supports_images` → `ChatMessage` carries the images as
    content parts (data URLs re-read from the PNGs on disk);
  - otherwise → each image's stored `description` is appended to the message
    text as `[Image: <description>]`.
- Missing descriptions (non-vision model, description not yet generated —
  including model switches mid-session): the send is **deferred**: the image
  model is called in the background for every missing description
  (status: `understanding image…`), results are persisted, then the send
  proceeds automatically. Vision-model sends never wait.
- `ChatMessage` gains `images: Vec<String>` (data URLs); serialization emits
  the plain string `content` when empty (wire format unchanged for all
  existing paths) or an OpenAI-style parts array when not.

### Image model (formerly "transcriber")
- Same setting/db key (`transcriber_model`), relabeled "image model" in
  `/config`; same default `google/gemini-2.5-flash-lite`.
- New prompt: understand, not transcribe — what the image is, layout/structure,
  key entities and their relationships, all visible text verbatim, notable
  visual details. Output a description another model can reason from.
- Empty image model + non-vision chat model + attached image → status tells
  the user to pick an image model or a vision model; send is blocked until
  resolved (attachment can be cleared with Esc).

### UI
- History pane: messages with images render an `🖼 image` marker line above
  the text.
- Model picker: vision-capable models get a small indicator (e.g. `⊡`),
  consistent with how reasoning support is surfaced (if/where it is).

## Out of scope
- Image files imported into the space fileset (images remain unsupported
  there; this feature is about conversation attachments).
- Accurate image token accounting in the context estimate (known
  underestimate; noted in code).
- Non-PNG wire formats (clipboard is always re-encoded PNG).

## Verification
- Unit: modality parsing, ChatMessage parts serialization (empty → string,
  non-empty → parts array), message_images roundtrip incl. description
  backfill, history assembly both paths (vision/parts vs description text),
  deferred-send state machine.
- Manual: paste screenshot → chip appears; send to a vision model (sees it,
  and again next turn); switch to a non-vision model mid-session → next send
  defers, describes once, injects description; reopen session → images
  reload from disk.
