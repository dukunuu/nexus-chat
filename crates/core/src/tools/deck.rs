//! The `deck` tool: confined, atomic deck actions. Studio and the model share
//! the same `*.deck.json` and `*.deck.json.comments.json` files, so every
//! write here shows up in an open studio window within half a second.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};

use crate::deck::feedback::{self, Anchor, Edit, Message, Role, Severity, Thread};
use crate::deck::suggest::{self, RawSuggestion, Suggestion};
use crate::deck::{Block, CheckError, Content, Deck, Op, render};

use super::ToolBox;

/// Slides shown as individual images; more become one contact sheet.
const MAX_SEPARATE_IMAGES: usize = 4;
/// Slides on one contact sheet.
const MAX_SHEET_SLIDES: usize = 12;
const MAX_COMMENT_CHARS: usize = 500;
const MAX_THREADS: usize = 200;
/// How much of a block's text `read` returns.
const READ_TEXT_CHARS: usize = 400;
const SHORT_REVISION: usize = 12;

fn slug(title: &str) -> String {
    let words = title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join("-");
    if words.is_empty() {
        "untitled".into()
    } else {
        words.chars().take(80).collect()
    }
}

/// Resolve `input` (relative to the space files folder) to a deck file under
/// `<files>/decks/`, rejecting escapes, odd characters and symlinks. Also used
/// by the terminal when it opens a linked canvas.
pub(crate) fn confined(files: &Path, input: &str) -> Result<PathBuf> {
    let relative = Path::new(input);
    ensure!(
        !relative.is_absolute()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "path must stay inside the space files, under decks/"
    );
    let name = relative.file_name().map(|n| n.to_string_lossy());
    ensure!(
        relative.starts_with("decks") && name.is_some_and(|n| n.ends_with(".deck.json")),
        "path must look like decks/name.deck.json"
    );
    ensure!(
        input
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/')),
        "use only letters, digits, - _ . and / in deck paths"
    );
    ensure!(
        !input.split('/').any(|part| part.starts_with('.')),
        "deck paths cannot contain hidden folders"
    );
    std::fs::create_dir_all(files)?;
    let mut path = files.canonicalize()?;
    for part in relative.components() {
        path.push(part);
        ensure!(
            !path.is_symlink(),
            "path escapes the space files through a symlink"
        );
    }
    Ok(path)
}

fn located(errors: &[CheckError]) -> String {
    errors
        .iter()
        .map(|e| format!("{}: {}", e.path, e.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn load(path: &Path) -> Result<Deck> {
    let bytes = std::fs::read(path).with_context(|| {
        format!(
            "no deck at {}; create it first",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    })?;
    ensure!(
        bytes.len() <= crate::deck::MAX_SOURCE,
        "deck file is too large"
    );
    let source = String::from_utf8(bytes).context("deck file is not UTF-8")?;
    Deck::check(&source)
        .map_err(|errors| anyhow::anyhow!("deck file is invalid: {}", located(&errors)))
}

fn save(path: &Path, deck: &Deck) -> Result<()> {
    crate::deck::write_atomic(path, &serde_json::to_vec_pretty(deck)?)
}

/// Fill hashes and insertion ids, check every operation against `deck`, and
/// return the prepared edit; no write happens here.
fn prepare(deck: &Deck, ops: &Value) -> Result<Suggestion> {
    let ops = ops.as_array().context("ops must be an array")?.clone();
    ensure!(!ops.is_empty(), "ops must contain at least one operation");
    let (mut valid, errors) = suggest::prepare(
        deck,
        vec![RawSuggestion {
            why: "Suggested edit".into(),
            ops,
        }],
    );
    valid.pop().ok_or_else(|| {
        anyhow::anyhow!(
            "{}. Nothing was written; read the deck and correct that operation.",
            located(&errors)
        )
    })
}

/// Slides whose content differs, in deck order. A theme change touches all.
fn changed_slides(before: &Deck, after: &Deck) -> Vec<String> {
    after
        .slides
        .iter()
        .filter(|slide| {
            before.theme != after.theme
                || before.slides.iter().find(|old| old.id == slide.id) != Some(*slide)
        })
        .map(|slide| slide.id.clone())
        .collect()
}

fn slide_number(deck: &Deck, id: &str) -> Result<usize> {
    deck.slides
        .iter()
        .position(|slide| slide.id == id)
        .map(|index| index + 1)
        .with_context(|| {
            let known = deck
                .slides
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("unknown slide {id:?}; slides are: {known}")
        })
}

/// `1 slide`, `3 slides`.
fn slides(count: usize) -> String {
    format!("{count} slide{}", if count == 1 { "" } else { "s" })
}

fn short(revision: &str) -> &str {
    &revision[..SHORT_REVISION]
}

fn clip(text: &str, chars: usize) -> String {
    let flat = text.replace('\n', " / ");
    if flat.chars().count() > chars {
        format!("{}…", flat.chars().take(chars).collect::<String>())
    } else {
        flat
    }
}

/// A lowercase enum name (`Xxl` becomes `xxl`).
fn name(value: &impl std::fmt::Debug) -> String {
    format!("{value:?}").to_lowercase()
}

/// One line per operation, for the edit result.
fn describe(op: &Op, before: &Deck) -> String {
    let number =
        |id: &str| slide_number(before, id).map_or_else(|_| id.to_string(), |n| n.to_string());
    match op {
        Op::AddSlide { slide, .. } => {
            format!("add_slide {} ({} blocks)", slide.id, slide.blocks.len())
        }
        Op::DeleteSlide { id } => format!("delete_slide {id} (was slide {})", number(id)),
        Op::MoveSlide { id, to } => format!("move_slide {id} to position {}", to + 1),
        Op::SetLayout { slide, layout } => {
            format!("set_layout slide {} to {}", number(slide), name(layout))
        }
        Op::SetNotes { slide, .. } => format!("set_notes slide {}", number(slide)),
        Op::InsertBlock { slide, block } => {
            format!("insert_block {} on slide {}", block.id, number(slide))
        }
        Op::UpdateBlock {
            slide, id, fields, ..
        } => format!(
            "update_block {id} on slide {}: {}",
            number(slide),
            fields.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
        Op::DeleteBlock { slide, id } => format!("delete_block {id} on slide {}", number(slide)),
        Op::Restyle { target, .. } => format!("restyle {target}"),
        Op::SetTheme { .. } => "set_theme".into(),
    }
}

fn block_line(block: &Block) -> String {
    let rect = format!("[{},{} {}x{}]", block.x, block.y, block.w, block.h);
    let style = format!(
        "{}/{}{}",
        name(&block.style.size),
        name(&block.style.align),
        block
            .style
            .color
            .as_ref()
            .map_or(String::new(), |c| format!("/{c}"))
    );
    let detail = match &block.content {
        Content::Text { text } => format!("text {style}: {}", clip(text, READ_TEXT_CHARS)),
        Content::Code { language, source } => {
            format!("code {language} {style}: {}", clip(source, READ_TEXT_CHARS))
        }
        Content::Table { rows } => {
            let cells = rows
                .iter()
                .map(|row| row.join(" | "))
                .collect::<Vec<_>>()
                .join(" ; ");
            format!(
                "table {}x{} {style}: {}",
                rows.len(),
                rows.iter().map(Vec::len).max().unwrap_or(0),
                clip(&cells, READ_TEXT_CHARS)
            )
        }
        Content::Image { src, fit } => format!("image {src} {}", name(fit)),
        Content::Shape {
            shape,
            fill,
            stroke,
        } => format!("shape {} fill={fill} stroke={stroke}", name(shape)),
        Content::Chart {
            chart,
            labels,
            series,
        } => {
            let series = series
                .iter()
                .map(|s| {
                    let values = s.values.iter().map(f64::to_string).collect::<Vec<_>>();
                    format!("{}: {}", s.name, values.join(" "))
                })
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "chart {} labels=[{}] series=[{series}]",
                name(chart),
                labels.join(", ")
            )
        }
    };
    format!("  {} {rect} {detail}", block.id)
}

fn author(role: &Role) -> &'static str {
    if matches!(role, Role::Model) {
        "nexus"
    } else {
        "user"
    }
}

fn read_text(relative: &str, path: &Path, deck: &Deck) -> Result<String> {
    let mut out = format!(
        "{relative} · {:?} · {} · revision {}\n",
        deck.title,
        slides(deck.slides.len()),
        short(&deck.revision())
    );
    let tokens = |map: &std::collections::BTreeMap<String, String>| {
        map.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let _ = writeln!(
        out,
        "theme colors {} · fonts {}",
        tokens(&deck.theme.colors),
        tokens(&deck.theme.fonts)
    );
    for (index, slide) in deck.slides.iter().enumerate() {
        let notes = if slide.notes.is_empty() {
            String::new()
        } else {
            format!(" · notes: {}", clip(&slide.notes, 160))
        };
        let _ = writeln!(
            out,
            "slide {} (id {}, layout {}){notes}",
            index + 1,
            slide.id,
            name(&slide.layout)
        );
        for block in &slide.blocks {
            let _ = writeln!(out, "{}", block_line(block));
        }
    }
    let threads = feedback::load(path)?;
    let open = threads.iter().filter(|t| t.resolved.is_none()).count();
    let _ = writeln!(out, "comment threads: {} ({open} open)", threads.len());
    for thread in &threads {
        let slide =
            slide_number(deck, &thread.anchor.slide).map_or_else(|_| "?".into(), |n| n.to_string());
        let block = thread
            .anchor
            .block
            .as_ref()
            .map_or(String::new(), |b| format!(" block {b}"));
        let state = if thread.resolved.is_some() {
            "resolved"
        } else {
            "open"
        };
        let first = thread.messages.first();
        let _ = writeln!(
            out,
            "  {} · slide {slide}{block} · {} · {state} · {} messages · {}: {}",
            thread.id,
            name(&thread.severity),
            thread.messages.len(),
            first.map_or("-", |m| author(&m.role)),
            first.map_or(String::new(), |m| clip(&m.text, 160)),
        );
        if thread.messages.len() > 1
            && let Some(last) = thread.messages.last()
        {
            let _ = writeln!(
                out,
                "    latest ({}): {}",
                author(&last.role),
                clip(&last.text, 160)
            );
        }
    }
    Ok(out)
}

/// A new deck: the title and no placeholder content, or the given slides.
fn new_deck(title: &str, slides: Option<&Value>) -> Result<Deck> {
    if let Some(slides) = slides {
        return Deck::check(&json!({"version": 1, "title": title, "slides": slides}).to_string())
            .map_err(|errors| anyhow::anyhow!("slides are invalid: {}", located(&errors)));
    }
    let mut deck = Deck::new_from_layouts(title);
    deck.slides.truncate(1);
    deck.slides[0].blocks.truncate(1);
    Ok(deck)
}

/// Image references the model sees: individual slides, or one contact sheet.
struct Shown {
    ids: Vec<String>,
    sheet: bool,
}

impl Shown {
    fn some(deck: &Deck, ids: Vec<String>) -> Self {
        if ids.is_empty() {
            Self::overview(deck)
        } else if ids.len() > MAX_SEPARATE_IMAGES {
            Self {
                ids: ids.into_iter().take(MAX_SHEET_SLIDES).collect(),
                sheet: true,
            }
        } else {
            Self { ids, sheet: false }
        }
    }

    /// The whole deck: a lone slide on its own, otherwise a contact sheet.
    fn overview(deck: &Deck) -> Self {
        Self {
            ids: deck
                .slides
                .iter()
                .take(MAX_SHEET_SLIDES)
                .map(|s| s.id.clone())
                .collect(),
            sheet: deck.slides.len() > 1,
        }
    }

    fn lines(&self, deck: &Deck, path: &Path, files: &Path) -> Result<String> {
        let mut out = String::new();
        if self.ids.is_empty() {
            return Ok(out);
        }
        if self.sheet {
            let file = render::render_cached(deck, path, files, &self.ids, true)?;
            let _ = writeln!(out, "![{} slides]({file})", self.ids.len());
        } else {
            for id in &self.ids {
                let file =
                    render::render_cached(deck, path, files, std::slice::from_ref(id), false)?;
                let _ = writeln!(out, "![slide {}]({file})", slide_number(deck, id)?);
            }
        }
        Ok(out)
    }

    /// `slide 3` for one slide, `4 slides` otherwise.
    fn label(&self, deck: &Deck) -> Result<String> {
        Ok(match self.ids.as_slice() {
            [id] if !self.sheet => format!("slide {}", slide_number(deck, id)?),
            ids => slides(ids.len()),
        })
    }
}

impl ToolBox {
    /// The slide studio is showing for `path`, if it is linked to this session.
    fn studio_focus(&self, path: &Path) -> Option<String> {
        let context = self.deck_link.lock().expect("deck link");
        let (focused, slide, _) = context.focus.as_ref()?;
        (focused.canonicalize().ok()? == path.canonicalize().ok()?).then(|| slide.clone())
    }

    /// Tell the terminal which slide studio should jump to.
    fn deck_changed(&self, path: &Path, slide: Option<String>) {
        if let Some(slide) = slide {
            let mut context = self.deck_link.lock().expect("deck link");
            context.changed.push((path.to_path_buf(), slide));
            context.notify.notify_one();
        }
    }

    /// Run one `deck` action (`deck_create` … `deck_resolve`).
    pub(super) fn deck_action(&self, action: &str, v: &Value) -> Result<String> {
        let relative = match (v["path"].as_str(), v["title"].as_str()) {
            (Some(path), _) => path.to_string(),
            (None, Some(title)) if action == "deck_create" => {
                format!("decks/{}.deck.json", slug(title))
            }
            _ => bail!("path is required, for example decks/talk.deck.json"),
        };
        let path = confined(&self.space_files_dir, &relative)?;
        match action {
            "deck_create" => self.deck_create(&relative, &path, v),
            "deck_read" => read_text(&relative, &path, &load(&path)?),
            "deck_view" => self.deck_view(&relative, &path, v),
            "deck_edit" => self.deck_edit(&relative, &path, v),
            "deck_comment" => deck_comment(&relative, &path, v),
            "deck_resolve" => deck_resolve(&relative, &path, v),
            _ => bail!("unknown deck action"),
        }
    }

    fn deck_create(&self, relative: &str, path: &Path, v: &Value) -> Result<String> {
        ensure!(
            !path.exists(),
            "{relative} already exists; read it, then edit"
        );
        let title = v["title"].as_str().context("title is required")?;
        let deck = new_deck(title, v.get("slides"))?;
        save(path, &deck)?;
        self.deck_changed(path, deck.slides.first().map(|s| s.id.clone()));
        Ok(format!(
            "created deck · {relative} · {} · revision {}\n{}",
            slides(deck.slides.len()),
            short(&deck.revision()),
            Shown::overview(&deck).lines(&deck, path, &self.space_files_dir)?
        ))
    }

    fn deck_view(&self, relative: &str, path: &Path, v: &Value) -> Result<String> {
        let deck = load(path)?;
        let shown = if let Some(ids) = v.get("slides") {
            let ids: Vec<String> = serde_json::from_value(ids.clone())
                .context("slides must be a list of slide ids, for example [\"s1\",\"s3\"]")?;
            ensure!(
                ids.len() <= MAX_SHEET_SLIDES,
                "view at most {MAX_SHEET_SLIDES} slides at once"
            );
            for id in &ids {
                slide_number(&deck, id)?;
            }
            Shown::some(&deck, ids)
        } else if let Some(id) = self
            .studio_focus(path)
            .filter(|id| slide_number(&deck, id).is_ok())
        {
            Shown::some(&deck, vec![id])
        } else {
            Shown::overview(&deck)
        };
        Ok(format!(
            "viewed {} · {relative} · revision {}\n{}",
            shown.label(&deck)?,
            short(&deck.revision()),
            shown.lines(&deck, path, &self.space_files_dir)?
        ))
    }

    fn deck_edit(&self, relative: &str, path: &Path, v: &Value) -> Result<String> {
        let deck = load(path)?;
        if let Some(revision) = v["revision"].as_str() {
            ensure!(
                revision.len() >= 8 && deck.revision().starts_with(revision),
                "stale revision: the deck is now at revision {}. Nothing was written; read it again before editing.",
                short(&deck.revision())
            );
        }
        let edit = prepare(&deck, &v["ops"])?;
        let next = crate::deck::apply(&deck, &edit.ops)?;
        let changed = changed_slides(&deck, &next);
        save(path, &next)?;
        let focus = changed
            .first()
            .or_else(|| next.slides.first().map(|s| &s.id))
            .cloned();
        self.deck_changed(path, focus);
        let shown = Shown::some(&next, changed);
        let mut out = format!(
            "edited {} · {relative} · revision {}\n",
            shown.label(&next)?,
            short(&next.revision())
        );
        for op in &edit.ops {
            let _ = writeln!(out, "- {}", describe(op, &deck));
        }
        out.push_str(&shown.lines(&next, path, &self.space_files_dir)?);
        Ok(out)
    }
}

fn deck_comment(relative: &str, path: &Path, v: &Value) -> Result<String> {
    let deck = load(path)?;
    let text = v["text"].as_str().context("text is required")?.trim();
    ensure!(
        !text.is_empty() && text.chars().count() <= MAX_COMMENT_CHARS,
        "a comment is one to two sentences, at most {MAX_COMMENT_CHARS} characters"
    );
    let mut threads = feedback::load(path)?;
    let edits = |anchor: &Anchor| -> Result<Vec<Edit>> {
        if v.get("ops").is_none() {
            return Ok(vec![]);
        }
        Ok(vec![Edit::new(&deck, prepare(&deck, &v["ops"])?, anchor)])
    };
    if let Some(id) = v["thread"].as_str() {
        let thread = threads
            .iter_mut()
            .find(|t| t.id == id)
            .context("unknown thread; read the deck for current thread ids")?;
        ensure!(thread.messages.len() < 100, "that thread is full");
        thread.anchor.check(&deck).map_err(anyhow::Error::msg)?;
        let edits = edits(&thread.anchor)?;
        thread
            .messages
            .push(Message::new(Role::Model, text.into(), edits));
        feedback::save(path, &threads)?;
        return Ok(format!("replied in thread {id} · {relative}\n"));
    }
    let slide = v["slide"]
        .as_str()
        .context("slide is required for a new comment (or pass thread to reply)")?;
    let severity: Severity = serde_json::from_value(v["severity"].clone())
        .context("severity is required for a new comment: must, should or consider")?;
    let block = v["block"].as_str().map(str::to_string);
    let rect = deck
        .slides
        .iter()
        .find(|s| s.id == slide)
        .and_then(|s| s.blocks.iter().find(|b| Some(&b.id) == block.as_ref()));
    let anchor = Anchor {
        slide: slide.into(),
        block,
        x: rect.map_or(2, |b| b.x),
        y: rect.map_or(2, |b| b.y),
    };
    anchor.check(&deck).map_err(anyhow::Error::msg)?;
    ensure!(
        threads.len() < MAX_THREADS,
        "this deck has {MAX_THREADS} threads; resolve some first"
    );
    let edits = edits(&anchor)?;
    let id = uuid::Uuid::new_v4().to_string();
    threads.push(Thread {
        id: id.clone(),
        anchor,
        severity,
        created: feedback::now(),
        resolved: None,
        messages: vec![Message::new(Role::Model, text.into(), edits)],
    });
    feedback::save(path, &threads)?;
    Ok(format!(
        "commented on slide {} · {relative}\nthread {id}\n",
        slide_number(&deck, slide)?
    ))
}

fn deck_resolve(relative: &str, path: &Path, v: &Value) -> Result<String> {
    let id = v["thread"].as_str().context("thread is required")?;
    let mut threads = feedback::load(path)?;
    threads
        .iter_mut()
        .find(|t| t.id == id)
        .context("unknown thread; read the deck for current thread ids")?
        .resolved = Some(feedback::now());
    feedback::save(path, &threads)?;
    Ok(format!("resolved thread {id} · {relative}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        tools: ToolBox,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("nexus-deck-{}", uuid::Uuid::new_v4()));
            let mut tools = ToolBox::new(
                root.join("skills"),
                None,
                None,
                "auto".into(),
                vec![],
                None,
                None,
                None,
            );
            tools.space_files_dir = root.join("files");
            Self { root, tools }
        }

        async fn run(&self, action: &str, mut fields: Value) -> String {
            fields["action"] = json!(action);
            self.tools.run("deck", &fields.to_string()).await.0
        }

        fn file(&self, relative: &str) -> PathBuf {
            self.tools.space_files_dir.join(relative)
        }

        /// The files named by `![alt](path)` lines in a result.
        fn images(&self, result: &str) -> Vec<PathBuf> {
            result
                .lines()
                .filter_map(|line| line.strip_prefix("!["))
                .filter_map(|line| line.split_once("](")?.1.strip_suffix(')'))
                .map(|relative| self.file(relative))
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const PATH: &str = "decks/test-deck.deck.json";

    #[tokio::test]
    async fn create_read_view_return_text_and_real_images() {
        let f = Fixture::new();
        let created = f.run("create", json!({"title": "Test deck"})).await;
        assert!(
            created.starts_with("created deck · decks/test-deck.deck.json"),
            "{created}"
        );
        let images = f.images(&created);
        assert_eq!(images.len(), 1, "{created}");
        assert!(image::open(&images[0]).is_ok());
        assert!(images[0].starts_with(f.file("decks/.renders")));
        let again = f.run("create", json!({"title": "Test deck"})).await;
        assert!(again.contains("already exists"), "{again}");

        let read = f.run("read", json!({"path": PATH})).await;
        assert!(
            read.contains("slide 1 (id s1") && read.contains("b1 [3,6 25x5] text"),
            "{read}"
        );
        assert!(
            read.contains("revision") && read.contains("comment threads: 0"),
            "{read}"
        );

        let view = f.run("view", json!({"path": PATH})).await;
        assert!(view.starts_with("viewed slide 1 ·"), "{view}");
        assert_eq!(f.images(&view).len(), 1);
        let missing = f
            .run("view", json!({"path": PATH, "slides": ["nope"]}))
            .await;
        assert!(
            missing.contains("unknown slide") && missing.contains("s1"),
            "{missing}"
        );
    }

    #[tokio::test]
    async fn create_accepts_initial_slides_and_rejects_invalid_ones() {
        let f = Fixture::new();
        let slides = json!([
            {"id": "a", "layout": "title", "blocks": [{"kind": "text", "x": 1, "y": 1, "w": 10, "h": 2, "text": "Hello"}]},
            {"id": "b", "layout": "blank", "blocks": []}
        ]);
        let created = f
            .run("create", json!({"title": "Talk", "slides": slides}))
            .await;
        assert!(created.contains("2 slides"), "{created}");
        let bad = f
            .run(
                "create",
                json!({"title": "Bad", "path": "decks/bad.deck.json", "slides": [{"layout": "nonsense"}]}),
            )
            .await;
        assert!(
            bad.contains("deck failed") && !f.file("decks/bad.deck.json").exists(),
            "{bad}"
        );
        let view = f
            .run(
                "view",
                json!({"path": "decks/talk.deck.json", "slides": ["a", "b", "a", "b", "a"]}),
            )
            .await;
        assert!(
            view.contains("viewed 5 slides") && f.images(&view).len() == 1,
            "{view}"
        );
    }

    #[tokio::test]
    async fn edit_is_atomic_stale_aware_located_and_shows_the_change() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck"})).await;
        let before = std::fs::read(f.file(PATH)).unwrap();
        let rejected = [
            json!({"path": PATH, "ops": [{"op": "update_block", "slide": "s1", "id": "missing", "fields": {"text": "x"}}]}),
            json!({"path": PATH, "ops": [{"op": "launch_missiles"}]}),
            json!({"path": PATH, "ops": []}),
            json!({"path": PATH, "revision": "deadbeefdeadbeef", "ops": [{"op": "delete_slide", "id": "s1"}]}),
        ];
        for fields in rejected {
            let error = f.run("edit", fields).await;
            assert!(error.starts_with("deck failed"), "{error}");
            assert_eq!(std::fs::read(f.file(PATH)).unwrap(), before);
        }
        let stale = f
            .run(
                "edit",
                json!({"path": PATH, "revision": "deadbeefdeadbeef", "ops": [{"op": "delete_slide", "id": "s1"}]}),
            )
            .await;
        assert!(
            stale.contains("stale revision") && stale.contains("read it again"),
            "{stale}"
        );
        let located = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [{"op": "update_block", "slide": "s1", "id": "missing", "fields": {"text": "x"}}]}),
            )
            .await;
        assert!(
            located.contains("ops[0]") && located.contains("Nothing was written"),
            "{located}"
        );

        let read = f.run("read", json!({"path": PATH})).await;
        let revision = read
            .split("revision ")
            .nth(1)
            .and_then(|rest| rest.lines().next())
            .unwrap()
            .to_string();
        let edited = f
            .run(
                "edit",
                json!({"path": PATH, "revision": revision, "ops": [
                    {"op": "update_block", "slide": "s1", "id": "b1", "fields": {"text": "New title"}},
                    {"op": "add_slide", "slide": {"layout": "blank", "blocks": [{"kind": "text", "x": 2, "y": 2, "w": 20, "h": 3, "text": "Added"}]}}
                ]}),
            )
            .await;
        assert!(edited.starts_with("edited 2 slides ·"), "{edited}");
        assert!(
            edited.contains("- update_block b1 on slide 1: text") && edited.contains("- add_slide"),
            "{edited}"
        );
        assert_eq!(f.images(&edited).len(), 2, "{edited}");
        let deck = Deck::check(&std::fs::read_to_string(f.file(PATH)).unwrap()).unwrap();
        assert_eq!(deck.slides.len(), 2);
        assert!(!deck.slides[1].id.is_empty() && !deck.slides[1].blocks[0].id.is_empty());

        let one = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [{"op": "set_notes", "slide": "s1", "notes": "n"}]}),
            )
            .await;
        assert!(one.starts_with("edited slide 1 ·"), "{one}");
        let deleted = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [{"op": "delete_slide", "id": "s1"}]}),
            )
            .await;
        assert!(deleted.starts_with("edited slide 1 ·"), "{deleted}");
    }

    #[tokio::test]
    async fn edits_and_creates_notify_the_linked_studio() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck"})).await;
        f.run(
            "edit",
            json!({"path": PATH, "ops": [{"op": "set_notes", "slide": "s1", "notes": "n"}]}),
        )
        .await;
        let changed = std::mem::take(&mut f.tools.deck_link.lock().unwrap().changed);
        assert_eq!(changed.len(), 2);
        assert!(
            changed
                .iter()
                .all(|(path, slide)| path.ends_with("test-deck.deck.json") && slide == "s1")
        );
    }

    #[tokio::test]
    async fn view_defaults_to_the_slide_studio_is_showing() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck", "slides": [
            {"id": "a", "blocks": [{"kind": "text", "x": 1, "y": 1, "w": 10, "h": 2, "text": "One"}]},
            {"id": "b", "blocks": [{"kind": "text", "x": 1, "y": 1, "w": 10, "h": 2, "text": "Two"}]}
        ]})).await;
        let path = "decks/test-deck.deck.json";
        let overview = f.run("view", json!({"path": path})).await;
        assert!(overview.starts_with("viewed 2 slides"), "{overview}");
        f.tools.deck_link.lock().unwrap().focus = Some((f.file(path), "b".into(), vec![]));
        let focused = f.run("view", json!({"path": path})).await;
        assert!(focused.starts_with("viewed slide 2"), "{focused}");
    }

    #[tokio::test]
    async fn comments_resolve_and_replies_share_studios_sidecar() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck"})).await;
        let comment = f
            .run(
                "comment",
                json!({"path": PATH, "slide": "s1", "block": "b1", "severity": "should",
                    "text": "Clarify the title so the audience knows the point.",
                    "ops": [{"op": "update_block", "slide": "s1", "id": "b1", "fields": {"text": "Clear title"}}]}),
            )
            .await;
        assert!(comment.starts_with("commented on slide 1"), "{comment}");
        let threads = feedback::load(&f.file(PATH)).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].messages[0].edits.len(), 1);
        let id = threads[0].id.clone();

        let reply = f
            .run(
                "comment",
                json!({"path": PATH, "thread": id, "text": "Updated."}),
            )
            .await;
        assert!(reply.starts_with("replied in thread"), "{reply}");
        assert_eq!(feedback::load(&f.file(PATH)).unwrap()[0].messages.len(), 2);
        let read = f.run("read", json!({"path": PATH})).await;
        assert!(
            read.contains("1 open") && read.contains("latest (nexus): Updated."),
            "{read}"
        );

        for bad in [
            json!({"path": PATH, "slide": "s1", "block": "nope", "severity": "must", "text": "x"}),
            json!({"path": PATH, "slide": "nope", "severity": "must", "text": "x"}),
            json!({"path": PATH, "slide": "s1", "severity": "loud", "text": "x"}),
            json!({"path": PATH, "slide": "s1", "severity": "must", "text": "x".repeat(501)}),
            json!({"path": PATH, "slide": "s1", "severity": "must", "text": "x", "ops": [{"op": "delete_slide", "id": "zz"}]}),
            json!({"path": PATH, "text": "no anchor"}),
        ] {
            let result = f.run("comment", bad).await;
            assert!(result.starts_with("deck failed"), "{result}");
        }
        assert_eq!(feedback::load(&f.file(PATH)).unwrap().len(), 1);

        let resolved = f.run("resolve", json!({"path": PATH, "thread": id})).await;
        assert!(resolved.contains("resolved thread"));
        assert!(feedback::load(&f.file(PATH)).unwrap()[0].resolved.is_some());
        let unknown = f
            .run("resolve", json!({"path": PATH, "thread": "nope"}))
            .await;
        assert!(unknown.contains("unknown thread"), "{unknown}");
    }

    #[tokio::test]
    async fn paths_cannot_leave_decks_or_pass_symlinks() {
        let f = Fixture::new();
        let files = &f.tools.space_files_dir;
        for bad in [
            "../bad.deck.json",
            "decks/../../bad.deck.json",
            "/tmp/bad.deck.json",
            "other/bad.deck.json",
            "decks/bad.json",
            "decks/.renders/x.deck.json",
            "decks/a b.deck.json",
            "decks/a)(b.deck.json",
        ] {
            assert!(confined(files, bad).is_err(), "{bad}");
            let result = f.run("create", json!({"title": "x", "path": bad})).await;
            assert!(result.starts_with("deck failed"), "{bad}: {result}");
        }
        assert!(confined(files, "decks/sub/ok-1.deck.json").is_ok());
        #[cfg(unix)]
        {
            std::fs::create_dir_all(files.join("decks")).unwrap();
            std::os::unix::fs::symlink(&f.root, files.join("decks/escape")).unwrap();
            assert!(confined(files, "decks/escape/bad.deck.json").is_err());
            let result = f
                .run(
                    "create",
                    json!({"title": "x", "path": "decks/escape/bad.deck.json"}),
                )
                .await;
            assert!(result.contains("symlink"), "{result}");
        }
    }

    #[test]
    fn read_only_actions_are_classified_for_batch_and_agent_mode() {
        for (args, read_only) in [
            (r#"{"action":"read","path":"decks/a.deck.json"}"#, true),
            (r#"{"action":"view","path":"decks/a.deck.json"}"#, true),
            (
                r#"{"action":"edit","path":"decks/a.deck.json","ops":[]}"#,
                false,
            ),
            (r#"{"action":"create","title":"x"}"#, false),
            (
                r#"{"action":"comment","path":"decks/a.deck.json","text":"x"}"#,
                false,
            ),
            (
                r#"{"action":"resolve","path":"decks/a.deck.json","thread":"t"}"#,
                false,
            ),
        ] {
            assert_eq!(
                crate::tools::is_read_only_tool("deck", args),
                read_only,
                "{args}"
            );
        }
    }
}
