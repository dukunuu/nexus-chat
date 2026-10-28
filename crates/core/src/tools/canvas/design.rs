//! Design actions and the deck's copied theme and consistency hints.
use super::*;
use crate::design::{self, Design};

impl ToolBox {
    fn canvas_import_design(&self, v: &Value) -> Result<String> {
        let files = &self.space_files_dir;
        let from = v["from"].as_str().context("from must name a space file")?;
        let source = design::storage::confined(files, from)?;
        let imported = design::template::import_file(files, &source, v["design"].as_str())?;
        let mut out = format!(
            "imported design {}\ncolors: {}\nfonts: {}\nstyles: {}\n",
            imported.slug,
            serde_json::to_string(&imported.design.colors)?,
            serde_json::to_string(&imported.design.fonts)?,
            serde_json::to_string(&imported.design.doc.text_styles)?
        );
        for (name, layout) in &imported.design.slide.layouts {
            let _ = writeln!(
                out,
                "layout {name}: {}",
                design::layouts::slots(layout)
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        for warning in &imported.warnings {
            let _ = writeln!(out, "warning: {warning}");
        }
        if let Some(sheet) = design::template::contact_sheet(files, &imported)? {
            let shown = sheet.strip_prefix(files.canonicalize()?).unwrap_or(&sheet);
            let _ = writeln!(out, "![template layouts]({})", shown.display());
        }
        Ok(out)
    }

    pub(super) fn canvas_design(&self, v: &Value) -> Result<String> {
        let files = &self.space_files_dir;
        let default = design::default_slug(files)?;
        let slug = v["design"].as_str().unwrap_or(&default);
        match v["op"].as_str() {
            Some("import") => self.canvas_import_design(v),
            Some("list") => {
                let mut out = String::from("designs\n");
                for slug in design::list(files)? {
                    let design = design::load(files, &slug)?;
                    let source = design.source.as_ref().map_or_else(
                        || {
                            if files.join(format!("design/{slug}.design.json")).exists() {
                                "space".into()
                            } else {
                                "built-in".into()
                            }
                        },
                        |s| format!("template {}", s.template),
                    );
                    let character = design::presets::catalog()
                        .into_iter()
                        .find(|(preset, _)| *preset == slug)
                        .filter(|_| source == "built-in")
                        .map(|(_, character)| format!(" · {character}"))
                        .unwrap_or_default();
                    let _ = writeln!(
                        out,
                        "{slug} · {source}{}{character}",
                        if slug == default { " · default" } else { "" }
                    );
                }
                if !out.contains(" · template ") {
                    out.push_str("templates: none yet\n");
                }
                out.push_str(self.art_once());
                Ok(out)
            }
            Some("read") => {
                let design = design::load(files, slug)?;
                let mut value = serde_json::to_value(&design)?;
                value["slide"]["layouts"] = json!(
                    design
                        .slide
                        .layouts
                        .iter()
                        .map(|(name, slide)| (name, design::layouts::slots(slide)))
                        .collect::<BTreeMap<_, _>>()
                );
                Ok(format!(
                    "design {slug} · revision {}\n{}\n",
                    short(&design.revision()),
                    serde_json::to_string(&value)?
                ))
            }
            Some("set_default") => {
                design::set_default(files, slug)?;
                Ok(format!("default design: {slug}\n"))
            }
            Some("edit") => {
                let base = design::load(files, slug)?;
                let patch = v["patch"].as_object().context("patch must be an object")?;
                for (key, value) in patch {
                    ensure!(
                        matches!(
                            key.as_str(),
                            "colors" | "fonts" | "font_files" | "scale" | "slide" | "doc"
                        ),
                        "patch.{key}: only tokens and text styles can be edited"
                    );
                    if matches!(key.as_str(), "slide" | "doc") {
                        ensure!(
                            value
                                .as_object()
                                .is_some_and(|o| o.keys().all(|k| k == "text_styles")),
                            "patch.{key}: only text_styles can be edited"
                        );
                    }
                }
                let mut value = serde_json::to_value(base)?;
                crate::doc::ops::merge_patch(&mut value, &v["patch"]);
                let next = Design::check(&value.to_string())
                    .map_err(|e| anyhow::anyhow!("{}", located(&e)))?;
                design::save(files, slug, &next)?;
                Ok(format!(
                    "edited design {slug} · revision {}\n",
                    short(&next.revision())
                ))
            }
            _ => bail!("design op must be list, read, import, edit or set_default"),
        }
    }

    pub(super) fn copy_deck_design(&self, deck: &mut Deck, v: &Value) -> Result<()> {
        let default = design::default_slug(&self.space_files_dir)?;
        let slug = v["design"].as_str().unwrap_or(&default);
        let design = design::load(&self.space_files_dir, slug)?;
        let styles = v["text_styles"].as_object().map(|styles| {
            deck.theme
                .text_styles
                .iter()
                .filter(|(name, _)| styles.contains_key(*name))
                .map(|(name, font)| (name.clone(), font.clone()))
                .collect::<BTreeMap<_, _>>()
        });
        deck.theme = design.slide_theme();
        if let Some(styles) = styles {
            deck.theme.text_styles.extend(styles);
        }
        deck.design = Some(slug.into());
        deck.design_rev = Some(design.revision());
        Ok(())
    }

    pub(super) fn append_design_lint(&self, out: &mut String, deck: &Deck) -> Result<()> {
        if let Some(line) = design::taste::summary(&design::taste::hints(deck)) {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            let _ = writeln!(out, "{line}");
        }
        if let Some(slug) = &deck.design {
            let design = design::load(&self.space_files_dir, slug)?;
            if let Some(line) = design::summary(&design::lint_deck(deck, &design)) {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                let _ = writeln!(out, "{line}");
            }
        }
        Ok(())
    }
}
