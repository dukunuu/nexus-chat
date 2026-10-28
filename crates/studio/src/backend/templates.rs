//! Template upload is shared by doc and deck windows; applying remains one undo step.
use super::{Backend, Command, Event, Path, Result, asset_root, doc, ensure};
use nexus_core::design;

impl Backend {
    fn imported_template(imported: design::template::Imported) -> Event {
        Event::TemplateImported {
            slug: imported.slug,
            name: imported.design.name,
            warnings: imported.warnings,
        }
    }

    pub(super) fn import_template(&self, path: &Path) -> Result<Event> {
        ensure!(path.is_absolute(), "template path must be absolute");
        Ok(Self::imported_template(design::template::import_file(
            &self.files_dir,
            path,
            None,
        )?))
    }

    pub(super) fn import_template_data(&self, name: &str, data: &str) -> Result<Event> {
        ensure!(
            data.len() as u64 <= nexus_core::deck::import::MAX_PPTX_BYTES * 2
                && data.len().is_multiple_of(2),
            "template exceeds size limit or has invalid encoding"
        );
        let bytes: Result<Vec<u8>> = data
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
            .collect();
        Ok(Self::imported_template(design::template::import_bytes(
            &self.files_dir,
            Path::new(name),
            &bytes?,
            None,
        )?))
    }

    pub(super) fn template_designs(&self) -> Result<Event> {
        let current = self
            .doc
            .as_ref()
            .map_or(self.deck.design.as_ref(), |s| s.doc.design.as_ref());
        let mut available = Vec::new();
        for slug in design::list(&self.files_dir)? {
            let design = design::load(&self.files_dir, &slug)?;
            available.push(doc::DesignInfo {
                current: current == Some(&slug),
                slug,
                name: design.name,
                colors: design.colors,
                fonts: design.fonts,
            });
        }
        Ok(Event::Designs { available })
    }

    pub(super) fn use_design(&mut self, slug: &str) -> Result<Event> {
        if self.doc.is_some() {
            return self.doc_execute(Command::DocApplyDesign {
                design: Some(slug.into()),
            });
        }
        let design = design::load(&self.files_dir, slug)?;
        design::template::materialize(&self.files_dir, &design, &asset_root(&self.path)?)?;
        let resolver = |name: &str| design::load(&self.files_dir, name);
        #[cfg_attr(test, allow(unused_mut))]
        let mut next = nexus_core::deck::ops::apply_with_resolver(
            &self.deck,
            &[nexus_core::deck::Op::ApplyDesign {
                design: Some(slug.into()),
            }],
            Some(&resolver),
        )?;
        // The design's Google Fonts join the deck, so the page and exports draw
        // them; a family that cannot be fetched draws with the fallback.
        // Tests never reach the network.
        #[cfg(not(test))]
        if let Ok(http) = nexus_core::deck::google_fonts::Http::new() {
            let installed = nexus_core::deck::text::installed_families();
            nexus_core::deck::google_fonts::provide_blocking(
                &http,
                &asset_root(&self.path)?,
                &mut next,
                &installed,
            );
        }
        self.replace(next)?;
        self.known_revision = self.deck.revision();
        Ok(self.event())
    }
}

#[cfg(test)]
mod tests;
