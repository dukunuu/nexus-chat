use super::*;
use crate::deck::import::xml::{self, El};
use crate::doc::test_support::{Temp, sample, thread};
use std::io::Read as _;
fn parts(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut parts = BTreeMap::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        parts.insert(entry.name().to_string(), bytes);
    }
    parts
}
fn xml(parts: &BTreeMap<String, Vec<u8>>, name: &str) -> El {
    xml::parse(std::str::from_utf8(&parts[name]).unwrap()).unwrap()
}
fn descendants<'a>(el: &'a El, name: &str) -> Vec<&'a El> {
    let mut out = Vec::new();
    el.descendants(name, &mut out);
    out
}
#[test]
fn deterministic_native_package_has_complete_relationships_types_styles_and_numbering() {
    let temp = Temp::new();
    temp.image();
    let doc = sample();
    let threads = vec![thread(&doc, false), thread(&doc, true)];
    let bytes = export(&doc, Some(&temp.0.join("assets")), &threads, &mut vec![]).unwrap();
    assert_eq!(
        bytes,
        export(&doc, Some(&temp.0.join("assets")), &threads, &mut vec![]).unwrap()
    );
    let parts = parts(&bytes);
    for (name, bytes) in &parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            xml::parse(std::str::from_utf8(bytes).unwrap())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
    for name in parts.keys().filter(|n| n.ends_with(".rels")) {
        let root = xml(&parts, name);
        let dir = name.rsplit_once("/_rels/").map_or("", |(dir, _)| dir);
        for rel in &root.kids {
            if rel.attr("TargetMode") == Some("External") {
                continue;
            }
            let target = rel.attr("Target").unwrap();
            let joined = if dir.is_empty() {
                target.into()
            } else {
                format!("{dir}/{target}")
            };
            assert!(parts.contains_key(&joined), "{name} -> {joined}");
        }
    }
    let types = xml(&parts, "[Content_Types].xml");
    for name in parts.keys().filter(|n| n.as_str() != "[Content_Types].xml") {
        let covered = types.kids.iter().any(|e| {
            e.attr("PartName") == Some(format!("/{name}").as_str())
                || e.attr("Extension") == name.rsplit('.').next()
        });
        assert!(covered, "missing content type: {name}");
    }
    let styles = xml(&parts, "word/styles.xml");
    let style_ids = styles
        .kids
        .iter()
        .filter_map(|s| s.attr_local("styleId"))
        .collect::<BTreeSet<_>>();
    for id in [
        "Title",
        "Heading1",
        "Heading2",
        "Heading3",
        "Normal",
        "Quote",
        "Caption",
        "Code",
        "CodeChar",
        "ListParagraph",
        "Hyperlink",
        "IntenseQuote",
    ] {
        assert!(style_ids.contains(id));
    }
    for (name, bytes) in &parts {
        if !name.ends_with(".xml") {
            continue;
        }
        let tree = xml::parse(std::str::from_utf8(bytes).unwrap()).unwrap();
        for reference in descendants(&tree, "pStyle")
            .into_iter()
            .chain(descendants(&tree, "rStyle"))
        {
            assert!(style_ids.contains(reference.attr_local("val").unwrap()));
        }
    }
    let numbering = xml(&parts, "word/numbering.xml");
    let number_ids = numbering
        .kids_named("num")
        .filter_map(|e| e.attr_local("numId"))
        .collect::<BTreeSet<_>>();
    let document = xml(&parts, "word/document.xml");
    for reference in descendants(&document, "numId") {
        assert!(number_ids.contains(reference.attr_local("val").unwrap()));
    }
    assert!(
        descendants(&document, "ilvl")
            .iter()
            .any(|e| e.attr_local("val") == Some("1"))
    );
    for abstract_num in numbering.kids_named("abstractNum") {
        assert_eq!(abstract_num.kids_named("lvl").count(), 4);
    }
    assert!(!descendants(&document, "tblHeader").is_empty());
    assert_eq!(descendants(&document, "blip").len(), 2);
    let comments = std::str::from_utf8(&parts["word/comments.xml"]).unwrap();
    assert!(comments.contains("open comment"));
    assert!(!comments.contains("closed comment"));
    assert_eq!(descendants(&document, "commentRangeStart").len(), 1);
    assert_eq!(descendants(&document, "commentRangeEnd").len(), 1);
    let footer = std::str::from_utf8(&parts["word/footer1.xml"]).unwrap();
    assert!(footer.contains("PAGE") && footer.contains("NUMPAGES"));
    assert!(
        parts.contains_key("word/footnotes.xml") && parts.contains_key("word/theme/theme1.xml")
    );
}
#[test]
fn docx_round_trip_keeps_native_block_essentials_and_anchored_comments() {
    let temp = Temp::new();
    temp.image();
    let doc = sample();
    let bytes = export(
        &doc,
        Some(&temp.0.join("assets")),
        &[thread(&doc, false)],
        &mut vec![],
    )
    .unwrap();
    let imported = crate::doc::import::docx::import(&bytes, &temp.0.join("import-assets")).unwrap();
    let result = &imported.doc;
    assert_eq!(result.title, doc.title);
    assert!(
        result
            .blocks
            .iter()
            .any(|b| matches!(&b.kind,Kind::Title { text } if text=="Research report"))
    );
    assert!(
        result
            .blocks
            .iter()
            .any(|b| matches!(&b.kind,Kind::Heading { level:1,text } if text=="Findings"))
    );
    assert!(
        result
            .blocks
            .iter()
            .any(|b| matches!(&b.kind,Kind::Quote { text } if text.contains("Clarity")))
    );
    assert!(
        result
            .blocks
            .iter()
            .any(|b| matches!(&b.kind,Kind::Callout { text,.. } if text.contains("preliminary")))
    );
    assert!(result.blocks.iter().any(|b| matches!(&b.kind,Kind::Code { text,.. } if text=="select region, revenue\n  from quarterly_results;")));
    assert!(result.blocks.iter().any(|b| matches!(&b.kind,Kind::Table { header:true,rows,align } if rows[1]==["Europe","4.1"] && align[1]==crate::deck::Align::Right)));
    assert!(result.blocks.iter().any(|b| matches!(&b.kind,Kind::List { ordered:true,items } if items[0].children[0].text=="Check the primary source")));
    assert!(result.blocks.iter().any(
        |b| matches!(&b.kind,Kind::List { items,.. } if items.iter().any(|i| i.checked==Some(true)))
    ));
    assert!(result.blocks.iter().any(
        |b| matches!(&b.kind,Kind::Image { caption,.. } if caption=="Figure 1. A color study.")
    ));
    assert_eq!(
        result
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, Kind::Image { .. }))
            .count(),
        2,
        "chart is an embedded PNG"
    );
    for kind in ["divider", "page_break", "toc"] {
        assert!(
            serde_json::to_string(result)
                .unwrap()
                .contains(&format!("\"kind\":\"{kind}\""))
        );
    }
    assert!(
        result
            .footnotes
            .values()
            .any(|t| t.contains("primary source"))
    );
    assert!(
        result
            .blocks
            .iter()
            .filter_map(Block::text)
            .any(|t| t.contains("**bold**")
                && t.contains("~~strike~~")
                && t.contains("https://example.test/report"))
    );
    assert_eq!(result.page.header, "Research report");
    assert_eq!(result.page.footer, "{page} / {pages}");
    assert_eq!(imported.threads.len(), 1);
    assert_eq!(
        imported.threads[0].anchor.quote,
        thread(&doc, false).anchor.quote
    );
    imported.threads[0].anchor.check(result).unwrap();
}
