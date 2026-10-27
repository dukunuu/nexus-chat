use super::*;
use crate::{
    deck::{self, Deck, Kind},
    doc::{self, Doc},
};

const POTX: &[u8] = include_bytes!("../../../tests/fixtures/templates/brand.potx");
const DOTX: &[u8] = include_bytes!("../../../tests/fixtures/templates/brand.dotx");
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("nexus-template-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn relationships_valid(parts: &package::Parts) {
    for (name, bytes) in parts.iter().filter(|(n, _)| n.ends_with(".rels")) {
        let source = std::str::from_utf8(bytes).unwrap();
        let root = deck::import::xml::parse(source).unwrap();
        let (directory, file) = name.rsplit_once("/_rels/").unwrap_or(("", ".rels"));
        let part = if name == "_rels/.rels" {
            String::new()
        } else {
            format!("{directory}/{}", file.trim_end_matches(".rels"))
        };
        for rel in root.kids_named("Relationship") {
            if rel.attr("TargetMode") == Some("External") {
                continue;
            }
            let target = rel.attr("Target").unwrap();
            let full = if target.starts_with('/') {
                target.trim_start_matches('/').to_string()
            } else {
                let mut pieces: Vec<_> = part
                    .rsplit_once('/')
                    .map_or("", |(dir, _)| dir)
                    .split('/')
                    .filter(|p| !p.is_empty())
                    .collect();
                for piece in target.split('/') {
                    match piece {
                        ".." => {
                            pieces.pop();
                        }
                        "." | "" => {}
                        other => pieces.push(other),
                    }
                }
                pieces.join("/")
            };
            assert!(parts.contains_key(&full), "{name} -> {target} ({full})");
        }
    }
}

#[test]
fn potx_extracts_brand_styles_slots_art_and_exports_real_placeholders() {
    let temp = Temp::new();
    let imported = import_bytes(&temp.0, Path::new("brand.potx"), POTX, None).unwrap();
    let design = &imported.design;
    assert_eq!(design.colors["accent"], "#12789a");
    assert_eq!(design.fonts["heading"], "Arial");
    assert_eq!(design.fonts["body"], "Calibri");
    assert_eq!(design.slide.text_styles["title"].size, Some(80.0));
    assert_eq!(design.slide.text_styles["body"].size, Some(44.0));
    assert_eq!(design.slide.size.w, 1920.0);
    assert_eq!(design.slide.layouts.len(), 3);
    assert_eq!(
        design.slide.layouts["title-slide"]
            .nodes()
            .into_iter()
            .filter(|n| !n.slot.is_empty())
            .map(|n| n.slot.as_str())
            .collect::<Vec<_>>(),
        ["title", "subtitle"]
    );
    let picture = &design.slide.layouts["picture-with-caption"];
    assert_eq!(
        crate::design::layouts::slots(picture)
            .into_iter()
            .collect::<Vec<_>>(),
        ["body", "image", "title"]
    );
    let image = picture
        .nodes()
        .into_iter()
        .find(|n| n.slot == "image")
        .unwrap();
    assert_eq!(
        image.position,
        deck::Position::Absolute { x: 120.0, y: 300.0 }
    );
    assert_eq!(image.sizing, deck::Sizing::fixed(960.0, 600.0));
    for layout in design.slide.layouts.values() {
        assert!(
            layout
                .nodes()
                .iter()
                .any(|n| n.locked && n.template_background && matches!(n.kind, Kind::Image { .. }))
        );
    }
    assert!(imported.warnings.iter().any(|w| w.contains("footer")));
    assert_eq!(
        std::fs::read(temp.0.join("templates/brand.potx")).unwrap(),
        POTX
    );
    assert_eq!(
        design.source.as_ref().unwrap().sha256,
        Sha256::digest(POTX)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                use std::fmt::Write as _;
                let _ = write!(hex, "{byte:02x}");
                hex
            })
    );
    let sheet = contact_sheet(&temp.0, &imported).unwrap().unwrap();
    let png = image::open(&sheet).unwrap();
    assert!(png.width() > 100 && png.height() > 100);
    if let Ok(directory) = std::env::var("NEXUS_RENDER_OUT") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::copy(&sheet, Path::new(&directory).join("p4-contact.png")).unwrap();
    }
    let assets = temp.0.join("decks/assets");
    materialize(&temp.0, design, &assets).unwrap();
    let resolver = |slug: &str| super::super::load(&temp.0, slug);
    let initial = Deck::check(&serde_json::json!({"version":2,"title":"Report","design":"brand","theme":design.slide_theme(),"slides":[]}).to_string()).unwrap();
    let Kind::Image {
        src: picture_src, ..
    } = &image.kind
    else {
        panic!("image slot");
    };
    let deck = deck::direct::prepare_with_resolver(&initial, vec![serde_json::json!({"op":"add_slide","layout":"title-slide","slots":{"title":"A real title","subtitle":"A real subtitle"}}), serde_json::json!({"op":"add_slide","layout":"title-and-content","slots":{"title":"Evidence","body":"The body"}}), serde_json::json!({"op":"add_slide","layout":"picture-with-caption","slots":{"title":"Picture","image":picture_src,"body":"Caption"}})], Some(&resolver)).unwrap().next;
    let export = || {
        export_deck(
            &deck,
            Some(&assets),
            &temp.0,
            deck::export::Format::Pptx,
            None,
        )
        .unwrap()
        .bytes
    };
    let bytes = export();
    assert_eq!(bytes, export());
    let parts = package::open(&bytes).unwrap();
    let template = package::open(POTX).unwrap();
    for (name, bytes) in template.iter().filter(|(n, _)| {
        n.starts_with("ppt/slideMasters/")
            || n.starts_with("ppt/slideLayouts/")
            || n.starts_with("ppt/theme/")
    }) {
        assert_eq!(&parts[name], bytes, "{name}");
    }
    assert!(!parts.keys().any(|n| n.contains("example")));
    let slide = package::text(&parts, "ppt/slides/slide1.xml").unwrap();
    assert!(slide.contains("type=\"ctrTitle\" idx=\"0\""));
    assert!(slide.contains("type=\"subTitle\" idx=\"1\""));
    assert!(slide.contains("A real title"));
    assert!(!slide.contains("Brand logo"));
    assert!(!slide.contains("<p:pic>"));
    let rels = package::text(&parts, "ppt/slides/_rels/slide2.xml.rels").unwrap();
    assert!(rels.contains("/ppt/slideLayouts/slideLayout2.xml"));
    assert!(
        package::text(&parts, "ppt/slides/slide3.xml")
            .unwrap()
            .contains("<p:ph type=\"pic\" idx=\"1\"/>")
    );
    assert!(
        package::text(&parts, "[Content_Types].xml")
            .unwrap()
            .contains("presentationml.presentation.main+xml")
    );
    relationships_valid(&parts);
    let plain = export_deck(
        &deck,
        Some(&assets),
        &temp.0,
        deck::export::Format::Pptx,
        Some("none"),
    )
    .unwrap();
    assert_eq!(
        plain.bytes,
        deck::export::export(
            &deck,
            Some(&assets),
            deck::export::Format::Pptx,
            deck::export::Options::default()
        )
        .unwrap()
        .bytes
    );
}

#[test]
fn dotx_resolves_localized_names_and_preserves_word_package_parts() {
    let temp = Temp::new();
    let imported = import_bytes(&temp.0, Path::new("report.dotx"), DOTX, None).unwrap();
    let design = &imported.design;
    assert_eq!(design.fonts["heading"], "Arial");
    assert_eq!(design.colors["accent"], "#12789a");
    assert_eq!(design.doc.text_styles["body"].size, Some(12.0));
    assert_eq!(design.doc.text_styles["h1"].size, Some(16.0));
    assert_eq!(design.doc.text_styles["h1"].weight, Some(700));
    assert_eq!(design.doc.page.size, doc::model::Paper::Letter);
    assert_eq!(design.doc.page.margins_pt, [54.0, 72.0, 54.0, 72.0]);
    assert_eq!(design.doc.page.header, "Self-made Company");
    assert_eq!(design.doc.page.footer, "Page {page}");
    assert!(
        imported
            .warnings
            .iter()
            .any(|w| w.contains("not mapped to design roles"))
    );
    let doc = Doc::check(&serde_json::json!({"version":1,"title":"Report","design":"report","theme":doc::model::Theme::from_design(design),"page":design.doc.page,"blocks":[{"id":"b1","kind":"heading","level":1,"text":"Localized"},{"id":"b2","kind":"paragraph","text":"Body"},{"id":"b3","kind":"list","ordered":false,"items":[{"text":"Bullet"}]},{"id":"b4","kind":"list","ordered":true,"items":[{"text":"Number"}]}]}).to_string()).unwrap();
    let export = || {
        export_doc(&doc, None, &[], &temp.0, doc::export::Format::Docx, None)
            .unwrap()
            .bytes
    };
    let bytes = export();
    assert_eq!(bytes, export());
    let parts = package::open(&bytes).unwrap();
    let original = package::open(DOTX).unwrap();
    for name in [
        "word/styles.xml",
        "word/header1.xml",
        "word/footer1.xml",
        "word/theme/theme1.xml",
        "word/numbering.xml",
    ] {
        assert_eq!(parts[name], original[name], "{name}");
    }
    let xml = package::text(&parts, "word/document.xml").unwrap();
    assert!(xml.contains("w:val=\"berschrift1\""));
    assert!(xml.contains("w:val=\"Corps\""));
    assert!(xml.contains("w:numId w:val=\"7\""));
    assert!(xml.contains("w:numId w:val=\"8\""));
    assert!(!xml.contains("EXAMPLE DOCUMENT"));
    assert!(
        package::text(&parts, "[Content_Types].xml")
            .unwrap()
            .contains("wordprocessingml.document.main+xml")
    );
    relationships_valid(&parts);
    let plain = export_doc(
        &doc,
        None,
        &[],
        &temp.0,
        doc::export::Format::Docx,
        Some("none"),
    )
    .unwrap();
    assert_eq!(
        plain.bytes,
        doc::export::export(&doc, None, &[], doc::export::Format::Docx)
            .unwrap()
            .bytes
    );
}

#[test]
fn missing_word_styles_and_numbering_are_added_without_changing_existing_styles() {
    let mut parts = package::open(DOTX).unwrap();
    let styles = package::text(&parts, "word/styles.xml")
        .unwrap()
        .to_string();
    let mut changed = styles.clone();
    for range in package::ranges(&styles, "style").into_iter().rev() {
        let el = deck::import::xml::parse(&styles[range.clone()]).unwrap();
        if el.attr_local("styleId") == Some("berschrift1") {
            changed.replace_range(range, "");
        }
    }
    parts.insert("word/styles.xml".into(), changed.clone().into_bytes());
    parts.insert(
        "word/numbering.xml".into(),
        b"<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"/>"
            .to_vec(),
    );
    let template = deck::export::zip(parts.into_iter().collect()).unwrap();
    let temp = Temp::new();
    import_bytes(&temp.0, Path::new("sparse.dotx"), &template, None).unwrap();
    let doc = Doc::check(r#"{"version":1,"title":"Fallback","design":"sparse","blocks":[{"kind":"heading","level":1,"text":"Heading"},{"kind":"list","ordered":true,"items":[{"text":"Number"}]}]}"#).unwrap();
    let output = export_doc(&doc, None, &[], &temp.0, doc::export::Format::Docx, None).unwrap();
    let parts = package::open(&output.bytes).unwrap();
    assert!(
        package::text(&parts, "word/styles.xml")
            .unwrap()
            .contains("w:styleId=\"Heading1\"")
    );
    let before = deck::import::xml::parse(&changed).unwrap();
    let after = package::text(&parts, "word/styles.xml").unwrap();
    for el in before.kids_named("style") {
        let id = el.attr_local("styleId").unwrap();
        let raw = package::ranges(&changed, "style")
            .into_iter()
            .map(|r| &changed[r])
            .find(|s| s.contains(&format!("w:styleId=\"{id}\"")))
            .unwrap();
        assert!(after.contains(raw));
    }
    assert!(
        package::text(&parts, "word/numbering.xml")
            .unwrap()
            .contains("w:numFmt w:val=\"decimal\"")
    );
    relationships_valid(&parts);
}

#[test]
fn refuses_macros_and_bounded_parts_without_writing_templates() {
    let temp = Temp::new();
    for ext in ["pptm", "potm", "docm", "dotm"] {
        assert!(
            import_bytes(&temp.0, Path::new(&format!("bad.{ext}")), POTX, None)
                .unwrap_err()
                .to_string()
                .contains("macro")
        );
    }
    let mut parts = package::open(POTX).unwrap();
    parts.insert("ppt/vbaProject.bin".into(), vec![0]);
    assert!(
        import_bytes(
            &temp.0,
            Path::new("disguised.potx"),
            &deck::export::zip(parts.into_iter().collect()).unwrap(),
            None
        )
        .is_err()
    );
    let oversized =
        deck::export::zip(vec![("huge.xml".into(), vec![b' '; 24 * 1024 * 1024 + 1])]).unwrap();
    assert!(
        package::open(&oversized)
            .unwrap_err()
            .to_string()
            .contains("too large")
    );
    assert!(!temp.0.join("templates").exists());
}

#[test]
fn placeholder_run_fonts_and_word_preformatted_style_names_are_retained() {
    let mut ppt = package::open(POTX).unwrap();
    let name = "ppt/slideLayouts/slideLayout1.xml";
    let layout = package::text(&ppt, name).unwrap().replace(
        "<a:p/>",
        "<a:p><a:r><a:rPr sz=\"3600\" i=\"1\"/><a:t>Sample</a:t></a:r></a:p>",
    );
    ppt.insert(name.into(), layout.into_bytes());
    let temp = Temp::new();
    let imported = import_bytes(
        &temp.0,
        Path::new("font.potx"),
        &deck::export::zip(ppt.into_iter().collect()).unwrap(),
        None,
    )
    .unwrap();
    let node = imported.design.slide.layouts["title-slide"]
        .nodes()
        .into_iter()
        .find(|n| n.slot == "title")
        .unwrap();
    let Kind::Text { font, .. } = &node.kind else {
        panic!("text slot");
    };
    assert_eq!(font.size, Some(72.0));
    assert_eq!(font.italic, Some(true));
    let mut word = package::open(DOTX).unwrap();
    let before = package::text(&word, "word/styles.xml").unwrap().to_string();
    let mut styles = before.clone();
    for range in package::ranges(&before, "style").into_iter().rev() {
        let raw = &before[range.clone()];
        if deck::import::xml::parse(raw).unwrap().attr_local("styleId") == Some("Code") {
            styles.replace_range(
                range,
                &raw.replace("w:styleId=\"Code\"", "w:styleId=\"Pre\"")
                    .replace("w:val=\"Code\"", "w:val=\"HTML Preformatted\"")
                    .replace("Calibri", "Courier New"),
            );
        }
    }
    word.insert("word/styles.xml".into(), styles.clone().into_bytes());
    import_bytes(
        &temp.0,
        Path::new("code.dotx"),
        &deck::export::zip(word.into_iter().collect()).unwrap(),
        None,
    )
    .unwrap();
    let doc = Doc::check(r#"{"version":1,"title":"Code","design":"code","blocks":[{"kind":"code","text":"select 1"}]}"#).unwrap();
    let output = export_doc(&doc, None, &[], &temp.0, doc::export::Format::Docx, None).unwrap();
    let parts = package::open(&output.bytes).unwrap();
    assert_eq!(package::text(&parts, "word/styles.xml").unwrap(), styles);
    assert!(
        package::text(&parts, "word/document.xml")
            .unwrap()
            .contains("w:pStyle w:val=\"Pre\"")
    );
}

#[test]
fn template_section_ids_are_not_remapped_with_generated_hyperlinks() {
    let mut parts = package::open(DOTX).unwrap();
    for name in ["word/document.xml", "word/_rels/document.xml.rels"] {
        let source = package::text(&parts, name)
            .unwrap()
            .replace("rHeader", "rId1");
        parts.insert(name.into(), source.into_bytes());
    }
    let temp = Temp::new();
    import_bytes(
        &temp.0,
        Path::new("collision.dotx"),
        &deck::export::zip(parts.into_iter().collect()).unwrap(),
        None,
    )
    .unwrap();
    let doc = Doc::check(r#"{"version":1,"title":"Links","design":"collision","blocks":[{"kind":"paragraph","text":"[Evidence](https://example.test)"}]}"#).unwrap();
    let output = export_doc(&doc, None, &[], &temp.0, doc::export::Format::Docx, None).unwrap();
    let parts = package::open(&output.bytes).unwrap();
    let source = package::text(&parts, "word/document.xml").unwrap();
    assert!(source.contains("w:headerReference w:type=\"default\" r:id=\"rId1\""));
    assert!(source.contains("w:hyperlink r:id=\"nexusrId1\""));
    relationships_valid(&parts);
}

#[test]
fn entry_and_file_limits_are_enforced_before_import() {
    let parts = (0..20_001)
        .map(|n| (format!("part{n}.bin"), vec![0]))
        .collect();
    let bytes = deck::export::zip(parts).unwrap();
    assert!(
        package::open(&bytes)
            .unwrap_err()
            .to_string()
            .contains("too many parts")
    );
    let temp = Temp::new();
    std::fs::create_dir_all(&temp.0).unwrap();
    let source = temp.0.join("huge.potx");
    std::fs::File::create(&source)
        .unwrap()
        .set_len(deck::import::MAX_PPTX_BYTES + 1)
        .unwrap();
    assert!(
        import_file(&temp.0, &source, None)
            .unwrap_err()
            .to_string()
            .contains("250 MB")
    );
    assert!(!temp.0.join("templates").exists());
}

#[test]
fn word_spacing_and_art_degrade_with_warnings_and_external_images_are_not_followed() {
    let mut parts = package::open(DOTX).unwrap();
    let source = package::text(&parts, "word/styles.xml").unwrap().replace(
        "<w:rPr>",
        "<w:pPr><w:spacing w:before=\"120\"/></w:pPr><w:rPr>",
    );
    parts.insert("word/styles.xml".into(), source.into_bytes());
    let header = package::text(&parts, "word/header1.xml")
        .unwrap()
        .replace("</w:p>", "<w:r><w:drawing/></w:r></w:p>");
    parts.insert("word/header1.xml".into(), header.into_bytes());
    let bytes = deck::export::zip(parts.into_iter().collect()).unwrap();
    let temp = Temp::new();
    let imported = import_bytes(&temp.0, Path::new("warning.dotx"), &bytes, None).unwrap();
    assert!(imported.warnings.iter().any(|w| w.contains("spacing")));
    assert!(imported.warnings.iter().any(|w| w.contains("artwork")));
    let mut parts = package::open(POTX).unwrap();
    let rel = package::text(&parts, "ppt/slideMasters/_rels/slideMaster1.xml.rels")
        .unwrap()
        .replace(
            "Target=\"../media/logo.png\"",
            "Target=\"https://invalid.test/logo.png\" TargetMode=\"External\"",
        );
    parts.insert(
        "ppt/slideMasters/_rels/slideMaster1.xml.rels".into(),
        rel.into_bytes(),
    );
    let bytes = deck::export::zip(parts.into_iter().collect()).unwrap();
    let imported = import_bytes(&temp.0, Path::new("external.potx"), &bytes, None).unwrap();
    assert!(
        imported
            .warnings
            .iter()
            .any(|w| w.contains("Linked pictures"))
    );
}
