//! Flowing-document actions, page previews and offline file interchange.
use super::*;
use crate::doc::{self, Doc, Kind as BlockKind, Op as DocOp, feedback as comments};

const DOC_SCHEMA: &str = "\ndoc schema (sent once per session)\n\
Blocks: {id,kind,style?}. Text kinds title, paragraph, quote: text; heading: level 1–3,text; callout: tone info|warn|success|note,text; code: lang,text. list: ordered,items:[{text,checked?,children?}] (4 deep). table: header,align:[left|center|right],rows:[[text]]. image: src,alt,caption,width:inline|wide|full. chart: chart:{chart:bar|line|pie,labels,series:[{name,values}]}. divider,page_break,toc: no fields. Footnotes: doc.footnotes map.\n\
Ops: insert_blocks{after?,blocks}, insert_markdown{after?,markdown}, update_block{id,patch:merge patch}, delete_blocks{ids}, move_blocks{ids,after?}, replace_text{id,find,replace,occurrence?,item?} (exact source, all matches unless 1-based occurrence; list item path 2.1), set_page{patch}, set_title{title}, apply_design{design?}. Missing after appends. Ops atomic, ids automatic. revision from read permits a stale edit only when touched blocks stayed unchanged.\n\
Markdown: # title on create without title; headings #–###, paragraphs, -/1. lists, [ ]/[x] tasks, > quotes, > [!NOTE] callouts, fenced code, pipe tables, ![alt](src \"caption\"), --- divider; inline **bold**, *italic*, `code`, [links](url), ~~strike~~, [^footnotes]. read blocks:[heading-id] gives its whole section. Comments use block and optional plain-text quote; ops attach a reviewed suggestion; thread replies, resolve closes. Export format:docx|pdf|md|html, never overwriting. create from accepts .md/.docx/.dotx; approximations are listed. view renders pages; blocks filters pages containing those ids.\n";

impl ToolBox {
    pub(in crate::tools) fn is_doc(v: &Value) -> bool {
        v["path"].as_str().map_or(v["kind"] == "doc", |p| {
            p.starts_with("docs/") || p.ends_with(".doc.json")
        })
    }

    pub(in crate::tools) fn doc_schema_if_fits(&self, used: usize) -> &'static str {
        let mut context = self.deck_link.lock().expect("deck link");
        if context.doc_schema_shown || used + DOC_SCHEMA.chars().count() > read::RESULT_BUDGET {
            return "";
        }
        context.doc_schema_shown = true;
        DOC_SCHEMA
    }

    /// Tell the terminal which block studio should jump to. A doc with no blocks
    /// still opens, so the first block is an empty id then.
    fn doc_changed(&self, path: &Path, block: Option<String>) {
        self.deck_changed(path, Some(block.unwrap_or_default()));
    }

    pub(super) fn doc_action(&self, action: &str, v: &Value) -> Result<String> {
        let mut out = if action == "deck_create" {
            self.doc_create(v)?
        } else {
            let relative = v["path"]
                .as_str()
                .context("path is required: docs/name.doc.json")?;
            let path = doc::storage::confined(&self.space_files_dir, relative)?;
            let doc = doc::storage::load(&self.space_files_dir, relative)?;
            match action {
                "deck_read" => doc_read(relative, &path, &doc, v)?,
                "deck_edit" => self.doc_edit(relative, &path, &doc, v)?,
                "deck_comment" => self.doc_comment(relative, &path, &doc, v)?,
                "deck_resolve" => {
                    let id = v["thread"].as_str().context("thread is required")?;
                    let mut threads = comments::load(&path)?;
                    threads
                        .iter_mut()
                        .find(|t| t.id == id)
                        .context("unknown thread; read for current ids")?
                        .resolved = Some(comments::now());
                    comments::save(&path, &threads)?;
                    format!("resolved thread {id} · {relative}\n")
                }
                "deck_history" => self.doc_history(relative, &path, &doc, v)?,
                "deck_export" => return self.doc_export(relative, &path, &doc, v),
                "deck_view" => {
                    let blocks: Vec<String> = v
                        .get("blocks")
                        .map(|b| serde_json::from_value(b.clone()))
                        .transpose()
                        .context("blocks must be ids")?
                        .unwrap_or_default();
                    return doc::render::view(&doc, &path, &self.space_files_dir, &blocks);
                }
                "deck_fonts" => {
                    let mut deck = Deck::new_from_layouts("Fonts");
                    deck.theme.colors.clone_from(&doc.theme.colors);
                    deck.theme.fonts.clone_from(&doc.theme.fonts);
                    deck.theme.font_files.clone_from(&doc.theme.font_files);
                    fonts_text(&deck, v["text"].as_str())
                }
                _ => bail!("unknown doc action"),
            }
        };
        out.push_str(self.doc_schema_if_fits(out.chars().count()));
        Ok(out)
    }

    fn doc_create(&self, v: &Value) -> Result<String> {
        let imported = if let Some(from) = v["from"].as_str() {
            let source = assets::find_space_file(from, &self.space_files_dir)
                .context("no import file in the space files")?;
            let assets = crate::design::storage::confined(&self.space_files_dir, "docs/assets")?;
            Some(doc::import::file_confined(
                &source,
                &assets,
                &self.space_files_dir,
            )?)
        } else {
            None
        };
        let mut parsed =
            doc::markdown::parse_document(v["markdown"].as_str().unwrap_or_default(), false);
        if let Some(imported) = &imported {
            parsed.blocks.clone_from(&imported.doc.blocks);
            parsed.footnotes.clone_from(&imported.doc.footnotes);
        }
        let title = if let Some(title) = v["title"].as_str() {
            title.to_string()
        } else if let Some(imported) = &imported
            && Path::new(v["from"].as_str().unwrap_or_default())
                .extension()
                .is_some_and(|e| !e.eq_ignore_ascii_case("md"))
        {
            imported.doc.title.clone()
        } else if let Some(block) = parsed.blocks.iter_mut().find(|b| {
            matches!(
                b.kind,
                BlockKind::Heading { level: 1, .. } | BlockKind::Title { .. }
            )
        }) {
            let text = block.text().unwrap_or_default().to_string();
            block.kind = BlockKind::Title { text: text.clone() };
            doc::markdown::plain(&text)
        } else {
            "Untitled".into()
        };
        let relative = v["path"]
            .as_str()
            .map_or_else(|| format!("docs/{}.doc.json", slug(&title)), str::to_string);
        let path = doc::storage::confined(&self.space_files_dir, &relative)?;
        ensure!(
            !path.exists(),
            "{relative} already exists; read it, then edit it"
        );
        let default = crate::design::default_slug(&self.space_files_dir)?;
        let slug = v["design"].as_str().unwrap_or(&default);
        let design = crate::design::load(&self.space_files_dir, slug)?;
        let mut doc = Doc::from_design(&title, &design);
        doc.design = Some(slug.into());
        doc.blocks = parsed.blocks;
        doc.footnotes = parsed.footnotes;
        if let Some(imported) = &imported {
            doc.page.clone_from(&imported.doc.page);
        }
        self.doc_images(&mut doc, &path)?;
        let doc = Doc::check(&serde_json::to_string(&doc)?)
            .map_err(|e| anyhow::anyhow!("{}", located(&e)))?;
        doc::storage::save(&self.space_files_dir, &relative, &doc, Who::Model)?;
        self.doc_changed(&path, doc.blocks.first().map(|b| b.id.clone()));
        let mut out = format!(
            "created doc · {relative} · {} blocks · revision {}\n{}",
            doc.blocks.len(),
            short(&doc.revision()),
            doc::outline(&doc, 4500)
        );
        if let Some(imported) = imported {
            if !imported.threads.is_empty() {
                comments::save(&path, &imported.threads)?;
            }
            for warning in imported.warnings {
                let _ = writeln!(out, "warning: {warning}");
            }
        }
        Ok(out)
    }

    fn doc_images(&self, doc: &mut Doc, path: &Path) -> Result<()> {
        let assets_dir = path.parent().context("doc folder")?.join("assets");
        ensure!(!assets_dir.is_symlink(), "assets must not be a symlink");
        for block in &mut doc.blocks {
            if let BlockKind::Image { src, .. } = &mut block.kind
                && let Some(copy) =
                    assets::import_space_image(src, &self.space_files_dir, &assets_dir)?
            {
                *src = copy;
            }
        }
        Ok(())
    }

    fn doc_edit(&self, relative: &str, path: &Path, before: &Doc, v: &Value) -> Result<String> {
        let ops = v["ops"].as_array().context("ops must be an array")?.clone();
        let rebased = doc_revision(path, before, v["revision"].as_str(), &ops)?;
        let resolver = |slug: &str| crate::design::load(&self.space_files_dir, slug);
        let prepared =
            doc::direct::prepare_with_resolver(before, ops, Some(&resolver)).map_err(|e| {
                anyhow::anyhow!("Nothing was written. Problems to fix:{}", format_errors(&e))
            })?;
        let mut next = prepared.next;
        self.doc_images(&mut next, path)?;
        doc::storage::save(&self.space_files_dir, relative, &next, Who::Model)?;
        self.doc_changed(
            path,
            next.blocks
                .iter()
                .find(|block| before.block(&block.id) != Some(*block))
                .or_else(|| next.blocks.first())
                .map(|block| block.id.clone()),
        );
        let mut out = format!(
            "edited doc · {relative} · revision {} (was {})\n",
            short(&next.revision()),
            short(&before.revision())
        );
        if let Some(rebased) = rebased {
            let _ = writeln!(out, "{rebased}");
        }
        let mut counts = BTreeMap::<String, usize>::new();
        for op in &prepared.ops {
            let value = serde_json::to_value(op)?;
            let name = value["op"].as_str().unwrap_or_default();
            if prepared.ops.len() <= MAX_OP_LINES {
                let target = value["id"].as_str().map_or_else(
                    || value.get("ids").map_or(String::new(), Value::to_string),
                    str::to_string,
                );
                let _ = writeln!(out, "- {name} {target}");
            } else {
                *counts.entry(name.into()).or_default() += 1;
            }
        }
        if !counts.is_empty() {
            let _ = writeln!(
                out,
                "- {} operations: {}",
                prepared.ops.len(),
                counts
                    .iter()
                    .map(|(name, n)| format!("{n} {name}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let moved = prepared
            .ops
            .iter()
            .flat_map(|op| match op {
                DocOp::MoveBlocks { ids, .. } => ids.as_slice(),
                _ => &[],
            })
            .collect::<BTreeSet<_>>();
        let mut changed = next.clone();
        changed
            .blocks
            .retain(|block| moved.contains(&block.id) || before.block(&block.id) != Some(block));
        out.push_str(&doc::outline(&changed, 3000));
        for block in &before.blocks {
            if next.block(&block.id).is_none() {
                let _ = writeln!(out, "[{}] deleted", block.id);
            }
        }
        Ok(out)
    }

    fn doc_comment(&self, relative: &str, path: &Path, doc: &Doc, v: &Value) -> Result<String> {
        let text = v["text"].as_str().context("text is required")?.trim();
        ensure!(
            !text.is_empty() && text.chars().count() <= MAX_COMMENT_CHARS,
            "a comment is one to two sentences, at most {MAX_COMMENT_CHARS} characters"
        );
        let mut threads = comments::load(path)?;
        let anchor = if let Some(id) = v["thread"].as_str() {
            threads
                .iter()
                .find(|t| t.id == id)
                .context("unknown thread; read for current ids")?
                .anchor
                .clone()
        } else {
            let id = v["block"]
                .as_str()
                .context("block is required for a new comment")?;
            let block = doc.block(id).context("unknown block; read the outline")?;
            let quote = v["quote"].as_str().unwrap_or_default().to_string();
            let range = if quote.is_empty() {
                None
            } else {
                let text = block.plain_text();
                let byte = text
                    .find(&quote)
                    .context("quote is not in block plain text")?;
                let start = text[..byte].chars().count();
                Some(comments::Range {
                    start,
                    end: start + quote.chars().count(),
                })
            };
            comments::Anchor {
                block: Some(id.into()),
                range,
                quote,
            }
        };
        anchor.check(doc).map_err(anyhow::Error::msg)?;
        let mut edits = Vec::new();
        if let Some(ops) = v.get("ops") {
            let resolver = |slug: &str| crate::design::load(&self.space_files_dir, slug);
            let (mut valid, errors) = doc::suggest::prepare_with_resolver(
                doc,
                vec![doc::suggest::RawSuggestion {
                    why: "Suggested edit".into(),
                    ops: ops.as_array().context("ops must be an array")?.clone(),
                }],
                Some(&resolver),
            );
            let suggestion = valid.pop().ok_or_else(|| {
                anyhow::anyhow!(
                    "Nothing was written. Problems to fix:{}",
                    format_errors(&errors)
                )
            })?;
            edits.push(comments::Edit::new(doc, suggestion, &anchor));
        }
        let message = comments::Message::new_for(Role::Model, text.into(), edits);
        if let Some(id) = v["thread"].as_str() {
            let thread = threads
                .iter_mut()
                .find(|t| t.id == id)
                .context("unknown thread")?;
            ensure!(thread.messages.len() < 100, "that thread is full");
            thread.messages.push(message);
            comments::save(path, &threads)?;
            return Ok(format!("replied in thread {id} · {relative}\n"));
        }
        ensure!(
            threads.len() < MAX_THREADS,
            "this doc has {MAX_THREADS} threads; resolve some first"
        );
        let severity = serde_json::from_value(v["severity"].clone())
            .context("severity is required: must, should or consider")?;
        let id = uuid::Uuid::new_v4().to_string();
        threads.push(comments::Thread {
            id: id.clone(),
            anchor,
            severity,
            created: comments::now(),
            resolved: None,
            messages: vec![message],
        });
        comments::save(path, &threads)?;
        Ok(format!(
            "commented on {} · {relative}\nthread {id}\n",
            v["block"].as_str().unwrap_or_default()
        ))
    }

    fn doc_history(&self, relative: &str, path: &Path, doc: &Doc, v: &Value) -> Result<String> {
        if let Some(revision) = v["revision"].as_str() {
            let entry = history::find(path, revision)?;
            let snapshot: Doc = entry.load_for()?;
            doc::storage::save(&self.space_files_dir, relative, &snapshot, Who::Model)?;
            return Ok(format!(
                "restored {relative} to version {} · revision {} (was {})\nThis is a new write; the replaced version is in history.\n",
                entry.rev8,
                short(&snapshot.revision()),
                short(&doc.revision())
            ));
        }
        let entries = history::list(path)?;
        let mut out = format!(
            "history · {relative} · current revision {}\n",
            short(&doc.revision())
        );
        for entry in entries.iter().rev().take(HISTORY_LINES) {
            let snapshot: Doc = entry.load_for()?;
            let _ = writeln!(
                out,
                "{} · {} · replaced by {} · {} blocks",
                entry.rev8,
                entry.time(),
                entry.who,
                snapshot.blocks.len()
            );
        }
        if entries.is_empty() {
            out.push_str("no earlier versions kept yet\n");
        }
        out.push_str("restore with history and revision\n");
        Ok(out)
    }

    fn doc_export(&self, relative: &str, path: &Path, doc: &Doc, v: &Value) -> Result<String> {
        let format = v["format"]
            .as_str()
            .or_else(|| v["text"].as_str())
            .context("format is required: docx, pdf, md or html")?;
        let format: doc::export::Format = format.parse()?;
        if format != doc::export::Format::Md {
            let assets = path.parent().context("doc folder")?.join("assets");
            let output = crate::design::template::export_doc(
                doc,
                Some(&assets),
                &comments::load(path)?,
                &self.space_files_dir,
                format,
                v["template"].as_str(),
            )?;
            let written = crate::deck::export::write_export(&output, &self.space_files_dir, None)?;
            let root = self.space_files_dir.canonicalize()?;
            let mut out = format!(
                "exported {relative} → {}\n",
                written.strip_prefix(root)?.display()
            );
            for warning in output.warnings {
                let _ = writeln!(out, "warning: {warning}");
            }
            return Ok(out);
        }
        let assets_dir = crate::design::storage::confined(&self.space_files_dir, "exports/assets")?;
        let mut exported = doc.clone();
        for block in &mut exported.blocks {
            if let BlockKind::Image { src, .. } = &mut block.kind {
                if src.starts_with("https://") {
                    continue;
                }
                let from = path.parent().context("doc folder")?.join(&*src);
                let source =
                    assets::find_space_file(&from.to_string_lossy(), &self.space_files_dir)
                        .context("image is missing from the space files")?;
                ensure!(
                    std::fs::metadata(&source)?.len() <= assets::MAX_IMAGE_BYTES,
                    "image exceeds 16 MB"
                );
                *src = assets::store_image_bytes(
                    &std::fs::read(&source)?,
                    src,
                    &slug(src),
                    &assets_dir,
                )?;
            }
        }
        let output = crate::deck::export::Export {
            bytes: doc::markdown::export(&exported).into_bytes(),
            file_name: format!("{}.md", slug(&doc.title)),
            warnings: Vec::new(),
        };
        let written = crate::deck::export::write_export(&output, &self.space_files_dir, None)?;
        let root = self.space_files_dir.canonicalize()?;
        Ok(format!(
            "exported {relative} → {}\n",
            written.strip_prefix(root)?.display()
        ))
    }
}

fn doc_read(relative: &str, path: &Path, doc: &Doc, v: &Value) -> Result<String> {
    let mut out = format!(
        "doc · {relative} · {} blocks · revision {}\n",
        doc.blocks.len(),
        short(&doc.revision())
    );
    out.push_str(&doc::outline(doc, 4500));
    if let Some(blocks) = v.get("blocks") {
        let ids: Vec<String> =
            serde_json::from_value(blocks.clone()).context("blocks must be ids")?;
        for id in &ids {
            ensure!(
                doc.block(id).is_some(),
                "unknown block {id:?}; read the outline"
            );
        }
        out.push_str(&doc::read_blocks(doc, &ids));
        out.push('\n');
    }
    for thread in comments::load(path)?
        .iter()
        .filter(|t| t.resolved.is_none())
    {
        let _ = writeln!(
            out,
            "thread {} · {:?} · anchor {}",
            thread.id,
            thread.severity,
            serde_json::to_string(&thread.anchor)?
        );
        for message in &thread.messages {
            let _ = writeln!(out, "  {:?}: {}", message.role, message.text);
            for edit in &message.edits {
                let _ = writeln!(
                    out,
                    "  suggestion {} · {:?} · ops {}",
                    edit.proposal.id,
                    edit.status,
                    serde_json::to_string(&edit.proposal.ops)?
                );
            }
        }
    }
    Ok(out)
}

fn doc_revision(
    path: &Path,
    current: &Doc,
    given: Option<&str>,
    ops: &[Value],
) -> Result<Option<String>> {
    let Some(given) = given.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if given.len() >= 8 && current.revision().starts_with(given) {
        return Ok(None);
    }
    let stale = format!(
        "stale revision {given}: doc is now at revision {}.",
        short(&current.revision())
    );
    let entry = history::find(path, given).with_context(|| {
        format!("{stale} Nothing was written. Read the blocks you are editing; version not kept.")
    })?;
    let old: Doc = entry.load_for()?;
    let mut touched = BTreeSet::new();
    let mut fields = BTreeSet::new();
    for op in ops {
        for key in ["id", "after"] {
            if let Some(id) = op[key].as_str() {
                touched.insert(id.to_string());
            }
        }
        if let Some(ids) = op["ids"].as_array() {
            touched.extend(ids.iter().filter_map(Value::as_str).map(str::to_string));
        }
        match op["op"].as_str() {
            Some("set_title") => {
                if old.title != current.title {
                    fields.insert("title");
                }
            }
            Some("set_page") => {
                if old.page != current.page {
                    fields.insert("page");
                }
            }
            Some("apply_design") => {
                if old.theme != current.theme
                    || old.design != current.design
                    || old.design_rev != current.design_rev
                {
                    fields.insert("theme");
                }
                if old.page != current.page {
                    fields.insert("page");
                }
            }
            _ => {}
        }
    }
    let changed = touched
        .iter()
        .filter(|id| old.block(id) != current.block(id))
        .cloned()
        .chain(fields.into_iter().map(str::to_string))
        .collect::<Vec<_>>();
    ensure!(
        changed.is_empty(),
        "{stale} These changed since then, and the edit touches them: {}. Nothing was written; read those blocks before retrying.",
        changed.join(", ")
    );
    Ok(Some(format!(
        "rebased: touched blocks unchanged since {given}; applied to revision {}",
        short(&current.revision())
    )))
}
