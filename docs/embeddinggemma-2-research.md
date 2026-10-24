# Research: EmbeddingGemma 2 for local file and media search

**Status: research only.** No runtime, dependency, configuration or model changes.

**Checked:** 2026-10-23 against repository commit
`d5140072dcd31750bf6b74129d96174c21078116`, Google's documentation/model
card, and Ollama v0.40.9. These are source-based findings, not an inference
benchmark or an end-to-end compatibility test.

## Bottom line

**Yes: EmbeddingGemma 2 is a good candidate for a simpler, local-first embedding
backend.** It can unify text, image, audio and video retrieval without requiring
each medium to become text first. That is a retrieval capability, not video
playback, a YouTube downloader, OCR output or transcription. [S1–S4]

For **existing text-file search**, Nexus already has most of the integration:
a local model identifier and OpenAI-compatible embeddings transport. Ollama
publishes `embeddinggemma-2:270m`, so the first experiment need not introduce
Python, a new database or a new inference runtime. For **multimodal retrieval**,
Nexus needs richer embedding inputs and search results. Ollama v0.40.9 accepts
image/audio inputs, but explicitly rejects direct video input; Google's
Sentence Transformers path documents all four modalities. [S4–S8][N1–N4]

## 1. What EmbeddingGemma 2 is

Google DeepMind released it on **2026-10-06**. It is not the original
`google/embeddinggemma-300m`: version 2 is a Gemma 4-based, multimodal embedding
model, published as `google/embeddinggemma-2` under Apache 2.0. [S1–S3]

| Property | Verified specification |
|---|---|
| Inputs | Text/code, images, audio, video, and interleaved combinations |
| Output | One shared 768-dimensional embedding space |
| Text-only configuration | Approximately 270M parameters |
| Text + vision | Approximately 440M parameters |
| Text + audio | Approximately 570M parameters |
| All encoders | Approximately 740M parameters |
| Documented input budget | 8,192 tokens, shared across modalities |
| Languages | 100+ |
| Supported compact vectors | 512, 256 and 128 dimensions, in addition to 768 |
| Deployment | Local/offline consumer CPU/GPU inference; runtime-dependent |

The modular encoders let an application load only the modalities it needs.
The shared space enables a text query to retrieve a picture, recording or
video segment, rather than matching only its filename or generated caption.
These capabilities are documented by Google; their quality on Nexus files has
not been measured. [S2–S4]

### Quality and resource caveats

Google reports multilingual MTEB scores of **61.36 vs 61.15** for version 2 vs
version 1, and code MTEB **78.68 vs 68.76**. That supports a substantial code
improvement, not a claim that every text task improves dramatically. There is
no Nexus-specific comparison here against its default OpenAI embedding model.
[S2][N1]

At 256 dimensions Google's reported multilingual MTEB falls from 61.36 to
60.41, while its combined MMEB score falls from 59.01 to 56.24. At 128 dimensions
the multimodal score drops to 45.65. Start with 768 for a quality baseline;
evaluate compression instead of assuming it is free. [S2]

The launch blog's approximately 191 MB text-only / 567 MB multimodal active-RAM
figures are **quantized Pixel 11 Pro measurements**, not a RAM guarantee for
Ollama or PyTorch on a laptop. [S1]

## 2. What Nexus can already reuse

The current flow is:

1. Copy an imported file into its space and extract readable text. Images and
   scanned documents use OCR/caption paths; audio/video can use external
   transcription/conversion tools. Otherwise indexing can retain metadata.
   [N2][N5]
2. Split extracted text into approximately **40-line chunks**. This is a
   line-count limit, not a token-count guarantee. [N5]
3. Embed one file at a time, batching up to **64 chunks** per request. Inputs
   currently look like `file: {filename}\n{chunk}`. [N2]
4. Send a JSON array of strings through `POST {base}/embeddings` using
   `OpenRouter::embed`. A `local:` model identifier resolves to the configured
   local backend, independently of the chat provider. [N1][N4]
5. Cache float32 vectors in SQLite and cosine-rank matching chunks within the
   active space. Search returns up to eight snippets, falling back to keyword
   search if embeddings fail or no usable vectors exist. [N3][N6]

The SQLite vector format is already variable-length: 768-dimensional output
requires **no new vector database**. Current similarity is brute-force cosine,
not an ANN/vector-server dependency. [N6]

Changing `embedding_model` through `App::set_setting` already clears cached
vectors, detaches the old embedding receiver and starts re-embedding. Vectors
do not currently record a model/revision identity, however. Reusing a model
name after a weight, preprocessing or dimension change would need additional
cache-version handling. [N7][N6]

## 3. Integration routes

### A. Smallest first step: local text search through Ollama

Ollama publishes `embeddinggemma-2:270m`. With a compatible local Ollama server
configured, the candidate Nexus setting is:

```text
embedding_model = local:embeddinggemma-2:270m
```

That is a **configuration candidate inferred from both implementations**, not
an end-to-end tested recipe. Ollama documents `/v1/embeddings` with string-array
input; Nexus already makes that request. [S5][S6][N1][N4]

Before considering the integration complete:

- **Apply the trained query/document prefixes.** Search queries use
  `task: search result | query: {query}`; documents use
  `title: {filename} | text: {chunk}`. Nexus currently uses neither format.
  Apply these only for models that require them; verify runtime preprocessing
  so a prefix is not applied twice. [S2][N2][N3]
- **Bound input by tokens.** Forty long lines can exceed the documented 8K
  budget. Do not depend on silent runtime truncation. [S2][N5]
- **Validate output.** Enforce one finite, nonempty, correctly dimensioned
  vector per input and honor response indices. The current client reads
  returned array order and filters nonnumeric coordinates rather than
  strictly validating the response. [N1]
- **Test both ingestion and query embedding** with the same checkpoint,
  preprocessing and output dimension; rebuild old vectors. [S2][N7]

Keep the current 768-dimensional default initially. Adding optional dimensions
would require a request option and cache identity, not a new similarity engine.
Ollama supports `dimensions` and re-normalizes after truncating. Nexus's current
cosine calculation already divides by vector norms; normalization remains
important for interoperable vectors or a future dot-product index. [S6][S7][N6]

### B. Images and audio: reuse Ollama, expand the input type

In **Ollama v0.40.9 source**, `/api/embed` accepts a text string, a batch, or
objects with `text`, `image` and `audio` keys. Image/audio values are base64
blobs, not arbitrary filesystem paths. Its OpenAI embeddings middleware forwards
`input` into the same handler, although the public compatibility documentation
only lists text/token input types. Treat multimodal use of `/v1/embeddings` as a
runtime-specific extension to verify and capability-gate, not portable OpenAI
behavior. [S6–S8]

Nexus currently accepts only `Vec<String>` and indexes textual chunks. It would
need to pass approved media bytes, retain an image/audio segment locator with
each vector, and return that locator so a retrieved hit can be inspected.
Changing only the model setting cannot provide those features. [N1–N3][N6]

**Important limitation:** that released Ollama handler returns
`input.video not supported`. Its model catalog's multimodal label is not proof
that its API implements every capability of the original checkpoint. [S7]

### C. Full media support: Google's Sentence Transformers path

Google documents this dependency set:

```sh
pip install -U sentence-transformers transformers pillow soundfile torchcodec
```

Its examples call `model.encode({"image": ...})`, `{"audio": ...}` and
`{"video": ...}`, as well as mixed dictionaries and explicit interleaving.
A persistent local worker could expose those capabilities to Nexus without
reimplementing model preprocessing in Rust. That worker is a proposed option,
not an existing Nexus feature. It adds Python/model lifecycle and multimedia
dependencies, so it is not the simplest option for text-only use. [S4][N1]

The checkpoint's Sentence Transformers configuration requires **>=6.1.0** for
correct ordering of multimodal dictionary inputs. Google instructs using
**bfloat16 or float32, not float16**, because fp16 can produce NaNs or degraded
vectors. Pin and test compatible library versions rather than assuming every
older Transformers installation can load this architecture. [S2][S9]

## 4. What this simplifies—and what it does not

**Potential simplification:** search images and audio directly rather than
requiring an OCR/caption/transcript model solely to make them discoverable.
For visual documents, rendered page images can be searched in the same space
as text. Keep extracted text/OCR where users need exact quotations, reading,
accessibility or keyword fallback: an embedding is not readable evidence.
[S2][S4][N3][N5]

**Media chunking still exists.** The model card budgets approximately 327
seconds of audio or 58 video frames at its stated token rates, before adding
other content. The documented video processor defaults to 1 fps and a
32-frame cap, with configurable sampling/overflow behavior. Long recordings
need explicit segment boundaries and timestamps; a single vector for an
hour-long video is not a substitute for finding individual moments. [S2][S4]

**YouTube is a separate ingestion problem.** Google demonstrates direct media
files/URLs, not turning a YouTube watch-page URL into audiovisual input.
Nexus's existing YouTube fetch path retrieves captions or page text; it does
not feed video bytes to an embedding model. A watch-page URL embedded as text
represents the URL string, not the video. Playback and UI cards remain
independent of all of this. [S4][N8][N9]

## 5. Recommended order

1. **Prove local text retrieval with Ollama's 270M variant**, correct prefixes,
   bounded inputs and strict vector validation. Compare retrieval against the
   existing backend on representative Nexus files before changing defaults.
2. **Make embedding configuration explicit:** backend/model revision, query
   and document formatting, dimension and preprocessing version. Use that
   identity for cache invalidation, not just vector length or a display name.
3. **Add image/audio embedding inputs and source locators**, capability-gated
   by the runtime. Preserve original files and optional textual extraction.
4. **Add time-bounded video segments** through a verified full-video runtime
   or an explicitly evaluated frame/audio strategy. Do not claim Ollama's
   direct-video endpoint exists today.

This is a recommendation derived from the current code and the cited APIs,
not an implementation commitment. It does not require coupling file search
to the terminal's interactive-UI renderer.

## Sources

### External primary sources

- **S1:** [Google launch announcement, 2026-10-06](https://blog.google/innovation-and-ai/technology/developers-tools/embeddinggemma-2/).
- **S2:** [Google EmbeddingGemma 2 model card](https://gemma.ai/models/embeddinggemma/model-card-v2/).
- **S3:** [Google model overview](https://gemma.ai/models/embeddinggemma/).
- **S4:** [Google multimodal inference guide](https://gemma.ai/models/embeddinggemma/multimodal/).
- **S5:** [Ollama's text-only model tag](https://ollama.com/library/embeddinggemma-2:270m) and [full model family](https://ollama.com/library/embeddinggemma-2).
- **S6:** [Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility) and [embed API](https://docs.ollama.com/api/embed).
- **S7:** [Ollama v0.40.9 embedding handler, input parsing, truncation and normalization](https://github.com/ollama/ollama/blob/v0.40.9/server/routes.go#L1082-L1417).
- **S8:** [Ollama v0.40.9 embeddings middleware](https://github.com/ollama/ollama/blob/v0.40.9/middleware/openai.go#L398-L455) and [embedding input type](https://github.com/ollama/ollama/blob/v0.40.9/api/types.go#L646-L685).
- **S9:** [Google checkpoint's Sentence Transformers configuration, pinned revision](https://huggingface.co/google/embeddinggemma-2/blob/914f7f89142e33e77833254d9c9b90c3cef7303b/config_sentence_transformers.json).

The older `ai.google.dev/gemma/docs/embeddinggemma` pages now direct readers
to `gemma.ai`; the current documentation above was checked as well. Ollama's
catalog advertises a 256K context, whereas Google's model card specifies 8K.
Use the model card's 8K budget until longer-input quality is independently
validated; positional capacity/catalog metadata is not a retrieval-quality
guarantee. [S2][S5]

### Repository evidence at the baseline commit

- **N1:** [`provider/openrouter.rs`](../crates/core/src/provider/openrouter.rs): `default_embedding_model`, `embed`.
- **N2:** [`app/files.rs`](../crates/core/src/app/files.rs): `import_file`, `start_embedding`, `on_embed_done`.
- **N3:** [`tools.rs`](../crates/core/src/tools.rs): `search_files_impl`, `semantic_snippets`.
- **N4:** [`app/backends.rs`](../crates/core/src/app/backends.rs): `Backends::resolve`; [`provider/local.rs`](../crates/core/src/provider/local.rs): `LocalConfig::endpoint`.
- **N5:** [`extract/mod.rs`](../crates/core/src/extract/mod.rs): `to_text_bytes`, `chunk_lines`; [`extract/external.rs`](../crates/core/src/extract/external.rs): media conversion/transcription.
- **N6:** [`db.rs`](../crates/core/src/db.rs): `set_chunk_embeddings`, `semantic_chunks`, `cosine`.
- **N7:** [`app/settings.rs`](../crates/core/src/app/settings.rs): `set_setting` embedding invalidation.
- **N8:** [`tools.rs`](../crates/core/src/tools.rs): `fetch_youtube_transcript`.
- **N9:** [`image_viewer.rs`](../crates/tui/src/image_viewer.rs): `video_for`, `route`; [`interactive.rs`](../crates/core/src/interactive.rs): declarative UI runtime.
