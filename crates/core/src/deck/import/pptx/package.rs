//! The OPC package: a zip of parts joined by relationships.

use std::collections::HashMap;
use std::io::{Cursor, Read as _};

use anyhow::{Context as _, Result, bail};

use super::super::xml::{self, El};

/// One XML part may not inflate past this.
const MAX_XML: u64 = 24 * 1024 * 1024;
/// One media part may not inflate past this.
const MAX_MEDIA: u64 = 48 * 1024 * 1024;
/// Everything read from one package.
const MAX_TOTAL: u64 = 600 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;

/// A relationship from one part to another part or an external target.
#[derive(Clone, Debug)]
pub(crate) struct Rel {
    pub kind: String,
    /// The target part name (no leading slash) or the external URL.
    pub target: String,
    pub external: bool,
}

/// The relationships of one part, by id.
#[derive(Default, Debug)]
pub(crate) struct Rels(HashMap<String, Rel>);

impl Rels {
    pub fn get(&self, id: &str) -> Option<&Rel> {
        self.0.get(id)
    }

    /// The first internal target whose relationship type ends in `kind`.
    pub fn first(&self, kind: &str) -> Option<&Rel> {
        let mut found: Vec<(&String, &Rel)> = self
            .0
            .iter()
            .filter(|(_, r)| r.kind.ends_with(kind))
            .collect();
        found.sort_by(|a, b| a.0.cmp(b.0));
        found.first().map(|(_, r)| *r)
    }
}

pub(crate) struct Package {
    zip: zip::ZipArchive<Cursor<Vec<u8>>>,
    /// Lower-cased part name to the entry's real name.
    names: HashMap<String, String>,
    read: u64,
}

fn directory(part: &str) -> &str {
    part.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Resolve a relationship `target` against the part that holds it.
fn resolve(part: &str, target: &str) -> String {
    let target = target.split('#').next().unwrap_or(target);
    let mut segments: Vec<&str> = if let Some(absolute) = target.strip_prefix('/') {
        absolute.split('/').collect()
    } else {
        directory(part)
            .split('/')
            .filter(|s| !s.is_empty())
            .chain(target.split('/'))
            .collect()
    };
    let mut out: Vec<&str> = Vec::new();
    for segment in segments.drain(..) {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

impl Package {
    /// Open `bytes` as a zip package.
    ///
    /// # Errors
    /// When the bytes are not a zip archive, or hold implausibly many entries.
    pub fn open(bytes: &[u8]) -> Result<Self> {
        if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
            bail!(
                "this is an old binary PowerPoint file (.ppt); open it in PowerPoint and save it as .pptx first"
            );
        }
        let zip = zip::ZipArchive::new(Cursor::new(bytes.to_vec()))
            .map_err(|_| anyhow::anyhow!("not a .pptx file: it is not a zip package"))?;
        if zip.len() > MAX_ENTRIES {
            bail!("the package has too many parts ({})", zip.len());
        }
        let names = zip
            .file_names()
            .map(|n| (n.trim_start_matches('/').to_lowercase(), n.to_string()))
            .collect();
        Ok(Self {
            zip,
            names,
            read: 0,
        })
    }

    pub fn has(&self, part: &str) -> bool {
        self.names.contains_key(&part.to_lowercase())
    }

    fn bytes(&mut self, part: &str, cap: u64) -> Result<Vec<u8>> {
        let name = self
            .names
            .get(&part.to_lowercase())
            .with_context(|| format!("missing part {part}"))?
            .clone();
        let file = self.zip.by_name(&name)?;
        if file.size() > cap || self.read + file.size() > MAX_TOTAL {
            bail!("part {part} is too large");
        }
        let mut out = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
        file.take(cap + 1).read_to_end(&mut out)?;
        if out.len() as u64 > cap {
            bail!("part {part} is too large");
        }
        self.read += out.len() as u64;
        Ok(out)
    }

    /// A media part's bytes.
    pub fn media(&mut self, part: &str) -> Result<Vec<u8>> {
        self.bytes(part, MAX_MEDIA)
    }

    /// An XML part, parsed.
    pub fn xml(&mut self, part: &str) -> Result<El> {
        let bytes = self.bytes(part, MAX_XML)?;
        let text = String::from_utf8_lossy(&bytes);
        xml::parse(&text).with_context(|| format!("part {part} is not valid XML"))
    }

    /// The relationships of `part` (empty when it has none).
    pub fn rels(&mut self, part: &str) -> Rels {
        let rels_name = if part.is_empty() {
            "_rels/.rels".to_string()
        } else {
            format!(
                "{}/_rels/{}.rels",
                directory(part),
                part.rsplit('/').next().unwrap_or(part)
            )
            .trim_start_matches('/')
            .to_string()
        };
        let Ok(root) = self.xml(&rels_name) else {
            return Rels::default();
        };
        let mut map = HashMap::new();
        for rel in root.kids_named("Relationship") {
            let (Some(id), Some(kind), Some(target)) =
                (rel.attr("Id"), rel.attr("Type"), rel.attr("Target"))
            else {
                continue;
            };
            let external = rel.attr("TargetMode") == Some("External");
            let target = if external {
                target.to_string()
            } else {
                resolve(part, target)
            };
            map.insert(
                id.to_string(),
                Rel {
                    kind: kind.to_string(),
                    target,
                    external,
                },
            );
        }
        Rels(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_resolve_against_the_part_directory() {
        assert_eq!(
            resolve("ppt/slides/slide1.xml", "../media/a.png"),
            "ppt/media/a.png"
        );
        assert_eq!(
            resolve("ppt/presentation.xml", "slides/slide2.xml"),
            "ppt/slides/slide2.xml"
        );
        assert_eq!(
            resolve("ppt/slides/slide1.xml", "/ppt/theme/theme1.xml"),
            "ppt/theme/theme1.xml"
        );
        assert_eq!(resolve("ppt/slides/slide1.xml", "../../../x"), "x");
        assert_eq!(resolve("a.xml", "b.xml#frag"), "b.xml");
    }

    #[test]
    fn garbage_and_legacy_files_have_clear_errors() {
        let error = Package::open(b"hello").err().unwrap().to_string();
        assert!(error.contains("not a .pptx file"), "{error}");
        let old = [0xD0, 0xCF, 0x11, 0xE0, 0, 0, 0, 0];
        let error = Package::open(&old).err().unwrap().to_string();
        assert!(error.contains(".ppt"), "{error}");
    }
}
