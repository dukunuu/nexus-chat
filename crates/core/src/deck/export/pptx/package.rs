//! OPC parts, masters, theme and relationships for the `PowerPoint` writer.

// OPC part names are case sensitive, unlike user-selected extensions.
#![allow(clippy::case_sensitive_file_extension_comparisons)]

use super::{Deck, NS, WIDTH, escape, integer, rgb};
use anyhow::Result;
use std::collections::BTreeMap;
use std::fmt::Write as _;

const OFFICE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MIME: &str = "application/vnd.openxmlformats-officedocument.presentationml";

pub(super) fn root_group() -> &'static str {
    "<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>"
}

fn rels(entries: &[(&str, String)]) -> String {
    let entries = entries
        .iter()
        .enumerate()
        .fold(String::new(), |mut out, (i, (kind, target))| {
            let _ = write!(
                out,
                "<Relationship Id=\"rId{}\" Type=\"{OFFICE}/{kind}\" Target=\"{}\"/>",
                i + 1,
                escape(target)
            );
            out
        });
    format!(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{entries}</Relationships>"
    )
}

pub(super) struct Package {
    parts: BTreeMap<String, (String, Vec<u8>)>,
}

impl Package {
    fn xml(&mut self, name: &str, kind: &str, xml: &str) {
        self.parts.insert(
            name.into(),
            (
                format!("{MIME}.{kind}+xml"),
                format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>{xml}")
                    .into_bytes(),
            ),
        );
    }

    fn rels(&mut self, name: &str, entries: &[(&str, String)]) {
        self.parts.insert(
            name.into(),
            (
                "application/vnd.openxmlformats-package.relationships+xml".into(),
                rels(entries).into_bytes(),
            ),
        );
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn new(deck: &Deck) -> Self {
        let mut package = Self {
            parts: BTreeMap::new(),
        };
        package.rels(
            "_rels/.rels",
            &[
                ("officeDocument", "ppt/presentation.xml".into()),
                ("extended-properties", "docProps/app.xml".into()),
            ],
        );
        // Core properties use the OPC (not officeDocument) relationship namespace.
        let root = format!(
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{OFFICE}/officeDocument\" Target=\"ppt/presentation.xml\"/><Relationship Id=\"rId2\" Type=\"{OFFICE}/extended-properties\" Target=\"docProps/app.xml\"/><Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>"
        );
        package
            .parts
            .get_mut("_rels/.rels")
            .expect("root relationships")
            .1 = root.into_bytes();
        let slides = deck
            .slides
            .iter()
            .enumerate()
            .fold(String::new(), |mut out, (i, _)| {
                let _ = write!(out, "<p:sldId id=\"{}\" r:id=\"rId{}\"/>", 256 + i, i + 3);
                out
            });
        package.xml("ppt/presentation.xml", "presentation.main", &format!("<p:presentation {NS}><p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst><p:notesMasterIdLst><p:notesMasterId r:id=\"rId2\"/></p:notesMasterIdLst><p:sldIdLst>{slides}</p:sldIdLst><p:sldSz cx=\"12192000\" cy=\"{}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/><p:defaultTextStyle/></p:presentation>", integer(WIDTH * deck.theme.size.h / deck.theme.size.w)));
        let mut relationships = vec![
            ("slideMaster", "slideMasters/slideMaster1.xml".into()),
            ("notesMaster", "notesMasters/notesMaster1.xml".into()),
        ];
        relationships.extend(
            deck.slides
                .iter()
                .enumerate()
                .map(|(i, _)| ("slide", format!("slides/slide{}.xml", i + 1))),
        );
        package.rels("ppt/_rels/presentation.xml.rels", &relationships);
        let map = "<p:clrMap accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" bg1=\"lt1\" bg2=\"lt2\" folHlink=\"folHlink\" hlink=\"hlink\" tx1=\"dk1\" tx2=\"dk2\"/>";
        package.xml("ppt/slideMasters/slideMaster1.xml", "slideMaster", &format!("<p:sldMaster {NS}><p:cSld><p:spTree>{}</p:spTree></p:cSld>{map}<p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle/><p:bodyStyle/><p:otherStyle/></p:txStyles></p:sldMaster>", root_group()));
        package.rels(
            "ppt/slideMasters/_rels/slideMaster1.xml.rels",
            &[
                ("slideLayout", "../slideLayouts/slideLayout1.xml".into()),
                ("theme", "../theme/theme1.xml".into()),
            ],
        );
        package.xml("ppt/slideLayouts/slideLayout1.xml", "slideLayout", &format!("<p:sldLayout {NS} type=\"blank\" preserve=\"1\"><p:cSld name=\"Blank\"><p:spTree>{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>", root_group()));
        package.rels(
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
            &[("slideMaster", "../slideMasters/slideMaster1.xml".into())],
        );
        package.xml("ppt/notesMasters/notesMaster1.xml", "notesMaster", &format!("<p:notesMaster {NS}><p:cSld><p:spTree>{}</p:spTree></p:cSld>{map}<p:notesStyle><a:lvl1pPr><a:defRPr sz=\"1200\"/></a:lvl1pPr></p:notesStyle></p:notesMaster>", root_group()));
        package.rels(
            "ppt/notesMasters/_rels/notesMaster1.xml.rels",
            &[("theme", "../theme/theme2.xml".into())],
        );
        // Each master owns its theme part, as PowerPoint writes them.
        for name in ["ppt/theme/theme1.xml", "ppt/theme/theme2.xml"] {
            package.parts.insert(
                name.into(),
                (
                    "application/vnd.openxmlformats-officedocument.theme+xml".into(),
                    theme(deck).into_bytes(),
                ),
            );
        }
        let title = escape(&deck.title);
        package.parts.insert("docProps/core.xml".into(), ("application/vnd.openxmlformats-package.core-properties+xml".into(), format!("<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><dc:title>{title}</dc:title><dc:creator>nexus studio</dc:creator><dcterms:created xsi:type=\"dcterms:W3CDTF\">1970-01-01T00:00:00Z</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">1970-01-01T00:00:00Z</dcterms:modified></cp:coreProperties>").into_bytes()));
        let titles = deck.slides.iter().fold(String::new(), |mut out, s| {
            let _ = write!(
                out,
                "<vt:lpstr>{}</vt:lpstr>",
                escape(
                    s.name
                        .as_deref()
                        .or_else(|| s.nodes().into_iter().find_map(super::Node::text))
                        .unwrap_or(&deck.title)
                )
            );
            out
        });
        package.parts.insert("docProps/app.xml".into(), ("application/vnd.openxmlformats-officedocument.extended-properties+xml".into(), format!("<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\" xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"><Application>nexus studio</Application><PresentationFormat>Custom</PresentationFormat><Slides>{}</Slides><Notes>{}</Notes><TitlesOfParts><vt:vector size=\"{}\" baseType=\"lpstr\">{titles}</vt:vector></TitlesOfParts></Properties>", deck.slides.len(), deck.slides.len(), deck.slides.len()).into_bytes()));
        package
    }

    pub(super) fn slide(&mut self, n: usize, xml: &str, notes: &str, media: Vec<Vec<u8>>) {
        self.xml(&format!("ppt/slides/slide{n}.xml"), "slide", xml);
        let mut rel = vec![("slideLayout", "../slideLayouts/slideLayout1.xml".into())];
        for (i, bytes) in media.into_iter().enumerate() {
            let name = format!("slide{n}-{}.png", i + 1);
            rel.push(("image", format!("../media/{name}")));
            self.parts
                .insert(format!("ppt/media/{name}"), ("image/png".into(), bytes));
        }
        rel.push(("notesSlide", format!("../notesSlides/notesSlide{n}.xml")));
        self.rels(&format!("ppt/slides/_rels/slide{n}.xml.rels"), &rel);
        let paragraphs = notes.split('\n').fold(String::new(), |mut out, line| {
            let _ = write!(
                out,
                "<a:p><a:r><a:t xml:space=\"preserve\">{}</a:t></a:r></a:p>",
                escape(line)
            );
            out
        });
        self.xml(&format!("ppt/notesSlides/notesSlide{n}.xml"), "notesSlide", &format!("<p:notes {NS}><p:cSld><p:spTree>{}<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Speaker notes\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"body\" idx=\"1\"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{paragraphs}</p:txBody></p:sp></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>", root_group()));
        self.rels(
            &format!("ppt/notesSlides/_rels/notesSlide{n}.xml.rels"),
            &[
                ("notesMaster", "../notesMasters/notesMaster1.xml".into()),
                ("slide", format!("../slides/slide{n}.xml")),
            ],
        );
    }

    pub(super) fn finish(self) -> Result<Vec<u8>> {
        let overrides = self
            .parts
            .iter()
            .filter(|(name, _)| !name.ends_with(".rels"))
            .fold(String::new(), |mut out, (name, (mime, _))| {
                let _ = write!(
                    out,
                    "<Override PartName=\"/{name}\" ContentType=\"{mime}\"/>"
                );
                out
            });
        let types = format!(
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>{overrides}</Types>"
        );
        let mut parts = vec![("[Content_Types].xml".into(), types.into_bytes())];
        parts.extend(
            self.parts
                .into_iter()
                .map(|(name, (_, bytes))| (name, bytes)),
        );
        super::super::zip(parts)
    }
}

fn theme(deck: &Deck) -> String {
    let colors = [
        ("dk1", "fg"),
        ("lt1", "bg"),
        ("dk2", "muted"),
        ("lt2", "bg"),
        ("accent1", "accent"),
        ("accent2", "fg"),
        ("accent3", "muted"),
        ("accent4", "accent"),
        ("accent5", "muted"),
        ("accent6", "fg"),
        ("hlink", "accent"),
        ("folHlink", "muted"),
    ]
    .iter()
    .fold(String::new(), |mut out, (name, role)| {
        let _ = write!(
            out,
            "<a:{name}><a:srgbClr val=\"{}\"/></a:{name}>",
            rgb(&deck.theme, role).0
        );
        out
    });
    let font = |role| {
        let family = escape(deck.theme.family(role));
        format!(
            "<a:latin typeface=\"{family}\"/><a:ea typeface=\"{family}\"/><a:cs typeface=\"{family}\"/>"
        )
    };
    let fill = "<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>";
    let line = format!(
        "<a:ln w=\"12700\" cap=\"flat\" cmpd=\"sng\" algn=\"ctr\">{fill}<a:prstDash val=\"solid\"/></a:ln>"
    );
    format!(
        "<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"nexus\"><a:themeElements><a:clrScheme name=\"Deck colors\">{colors}</a:clrScheme><a:fontScheme name=\"Deck fonts\"><a:majorFont>{}</a:majorFont><a:minorFont>{}</a:minorFont></a:fontScheme><a:fmtScheme name=\"nexus\"><a:fillStyleLst>{}</a:fillStyleLst><a:lnStyleLst>{}</a:lnStyleLst><a:effectStyleLst>{}</a:effectStyleLst><a:bgFillStyleLst>{}</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>",
        font("heading"),
        font("body"),
        fill.repeat(3),
        line.repeat(3),
        "<a:effectStyle><a:effectLst/></a:effectStyle>".repeat(3),
        fill.repeat(3)
    )
}
