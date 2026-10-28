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
        relationships.extend([
            ("presProps", "presProps.xml".into()),
            ("viewProps", "viewProps.xml".into()),
            ("tableStyles", "tableStyles.xml".into()),
        ]);
        package.rels("ppt/_rels/presentation.xml.rels", &relationships);
        package.xml(
            "ppt/presProps.xml",
            "presProps",
            &format!(
                "<p:presentationPr {NS}><p:showPr loop=\"0\" useTimings=\"1\"/></p:presentationPr>"
            ),
        );
        package.xml("ppt/viewProps.xml", "viewProps", &format!("<p:viewPr {NS} lastView=\"sldView\"><p:normalViewPr/><p:slideViewPr><p:cSldViewPr><p:cViewPr varScale=\"1\"><p:scale><a:sx n=\"1\" d=\"1\"/><a:sy n=\"1\" d=\"1\"/></p:scale><p:origin x=\"0\" y=\"0\"/></p:cViewPr></p:cSldViewPr></p:slideViewPr><p:gridSpacing cx=\"91440\" cy=\"91440\"/></p:viewPr>"));
        package.parts.insert("ppt/tableStyles.xml".into(), ("application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml".into(), "<a:tblStyleLst xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" def=\"{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}\"/>".as_bytes().to_vec()));
        let map = "<p:clrMap accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" bg1=\"lt1\" bg2=\"lt2\" folHlink=\"folHlink\" hlink=\"hlink\" tx1=\"dk1\" tx2=\"dk2\"/>";
        let layout_ids = (1..=5).fold(String::new(), |mut out, n| {
            let _ = write!(
                out,
                "<p:sldLayoutId id=\"{}\" r:id=\"rId{n}\"/>",
                2_147_483_648_u64 + n
            );
            out
        });
        let text_styles = [ ("titleStyle", "title"), ("bodyStyle", "body"), ("otherStyle", "body") ].map(|(tag, style)| {
            let font = deck.theme.resolve_font(style, &crate::deck::Font::default());
            let size = integer(font.size * super::WIDTH_PT / deck.theme.size.w * 100.0);
            format!("<p:{tag}><a:lvl1pPr><a:defRPr sz=\"{size}\" b=\"{}\" i=\"{}\">{}<a:latin typeface=\"{}\"/></a:defRPr></a:lvl1pPr></p:{tag}>", u8::from(font.weight >= 600), u8::from(font.italic), super::fill(&deck.theme, Some(&font.color), 1.0), escape(super::google_font(deck.theme.family(&font.family))))
        }).join("");
        package.xml("ppt/slideMasters/slideMaster1.xml", "slideMaster", &format!("<p:sldMaster {NS}><p:cSld><p:spTree>{}</p:spTree></p:cSld>{map}<p:sldLayoutIdLst>{layout_ids}</p:sldLayoutIdLst><p:txStyles>{text_styles}</p:txStyles></p:sldMaster>", root_group()));
        let mut master_rels = (1..=5)
            .map(|n| ("slideLayout", format!("../slideLayouts/slideLayout{n}.xml")))
            .collect::<Vec<_>>();
        master_rels.push(("theme", "../theme/theme1.xml".into()));
        package.rels("ppt/slideMasters/_rels/slideMaster1.xml.rels", &master_rels);
        for (index, (name, kind, placeholders)) in [
            ("Blank", "blank", vec![]),
            ("Title", "title", vec![("ctrTitle", "0"), ("subTitle", "1")]),
            (
                "Title and Content",
                "tx",
                vec![("title", "0"), ("body", "1")],
            ),
            (
                "Section Header",
                "secHead",
                vec![("title", "0"), ("body", "1")],
            ),
            ("Title Only", "titleOnly", vec![("title", "0")]),
        ]
        .into_iter()
        .enumerate()
        {
            let n = index + 1;
            let shapes = placeholders.iter().enumerate().fold(String::new(), |mut out, (i, (kind, idx))| {
                let rect = crate::deck::Rect { x:deck.theme.size.w / 16.0, y:deck.theme.size.h * if i == 0 { 120.0 / 1080.0 } else { 320.0 / 1080.0 }, w:deck.theme.size.w * 7.0 / 8.0, h:deck.theme.size.h * if i == 0 { 160.0 / 1080.0 } else { 640.0 / 1080.0 } };
                let ph = crate::deck::model::Placeholder { kind:(*kind).into(), idx:(*idx).into() };
                let _ = write!(out, "<p:sp>{}<p:spPr>{}</p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p/></p:txBody></p:sp>", super::nv(u32::try_from(i + 2).expect("two placeholders"), kind, true, Some(&ph)), super::xfrm(rect, WIDTH / deck.theme.size.w, None, false));
                out
            });
            package.xml(&format!("ppt/slideLayouts/slideLayout{n}.xml"), "slideLayout", &format!("<p:sldLayout {NS} type=\"{kind}\" preserve=\"1\"><p:cSld name=\"{name}\"><p:spTree>{}{shapes}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>", root_group()));
            package.rels(
                &format!("ppt/slideLayouts/_rels/slideLayout{n}.xml.rels"),
                &[("slideMaster", "../slideMasters/slideMaster1.xml".into())],
            );
        }
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

    pub(super) fn slide(
        &mut self,
        n: usize,
        xml: &str,
        notes: &str,
        media: Vec<Vec<u8>>,
        layout: usize,
    ) {
        self.xml(&format!("ppt/slides/slide{n}.xml"), "slide", xml);
        let mut rel = vec![(
            "slideLayout",
            format!("../slideLayouts/slideLayout{layout}.xml"),
        )];
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
        ("lt2", "surface"),
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
            rgb(
                &deck.theme,
                if deck.theme.colors.contains_key(*name) {
                    name
                } else if *role == "surface" && !deck.theme.colors.contains_key("surface") {
                    "bg"
                } else {
                    role
                }
            )
            .0
        );
        out
    });
    let font = |role| {
        let family = escape(super::google_font(deck.theme.family(role)));
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
