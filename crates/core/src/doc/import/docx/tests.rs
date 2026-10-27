use super::*;
use crate::doc::test_support::Temp;
fn package(document: &str, extra: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let mut parts=vec![("word/document.xml".into(),format!("<w:document xmlns:w=\"urn:word\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>{document}</w:body></w:document>").into_bytes()),("word/_rels/document.xml.rels".into(),b"<Relationships><Relationship Id=\"s\" Type=\"urn:/styles\" Target=\"styles.xml\"/><Relationship Id=\"n\" Type=\"urn:/numbering\" Target=\"numbering.xml\"/></Relationships>".to_vec()),("word/styles.xml".into(),b"<styles><style styleId=\"Heading1\"><name val=\"heading 1\"/></style><style styleId=\"Deep\"><name val=\"Custom heading\"/><pPr><outlineLvl val=\"5\"/></pPr></style><style styleId=\"Code\"><name val=\"HTML Preformatted\"/></style></styles>".to_vec()),("word/numbering.xml".into(),b"<numbering><abstractNum abstractNumId=\"7\"><lvl ilvl=\"0\"><numFmt val=\"decimal\"/></lvl><lvl ilvl=\"1\"><numFmt val=\"decimal\"/></lvl></abstractNum><num numId=\"9\"><abstractNumId val=\"7\"/></num></numbering>".to_vec())];
    parts.extend(extra);
    crate::deck::export::zip(parts).unwrap()
}
#[test]
fn word_style_names_outline_levels_numbering_runs_and_merged_cells() {
    let bytes = package(
        "<w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t>Named heading</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val=\"Deep\"/></w:pPr><w:r><w:t>Deep heading</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"9\"/></w:numPr></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>First</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val=\"1\"/><w:numId w:val=\"9\"/></w:numPr></w:pPr><w:r><w:t>Nested</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr><w:p><w:r><w:t>Merged</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:pPr><w:pStyle w:val=\"Code\"/></w:pPr><w:r><w:t xml:space=\"preserve\">  code</w:t><w:br/><w:t>next</w:t></w:r></w:p>",
        vec![],
    );
    let temp = Temp::new();
    let imported = import(&bytes, &temp.0.join("assets")).unwrap();
    assert!(
        matches!(&imported.doc.blocks[0].kind,Kind::Heading { level:1,text } if text=="Named heading")
    );
    assert!(matches!(
        &imported.doc.blocks[1].kind,
        Kind::Heading { level: 3, .. }
    ));
    assert!(
        matches!(&imported.doc.blocks[2].kind,Kind::List { ordered:true,items } if items[0].text=="**First**" && items[0].children[0].text=="Nested")
    );
    assert!(
        matches!(&imported.doc.blocks[3].kind,Kind::Table { rows,.. } if rows[0]==["Merged","Merged"])
    );
    assert!(matches!(&imported.doc.blocks[4].kind,Kind::Code { text,.. } if text=="  code\nnext"));
    assert!(imported.warnings.iter().any(|w| w.contains("Merged")));
}
#[test]
fn macro_packages_and_extensions_and_bounded_xml_are_refused() {
    let temp = Temp::new();
    let bytes = package("", vec![("word/vbaProject.bin".into(), vec![1, 2, 3])]);
    assert!(
        import(&bytes, &temp.0.join("assets"))
            .unwrap_err()
            .to_string()
            .contains("macro")
    );
    for name in ["x.docm", "x.dotm"] {
        let path = temp.0.join(name);
        std::fs::write(&path, []).unwrap();
        assert!(
            super::super::file(&path, &temp.0.join("assets"))
                .unwrap_err()
                .to_string()
                .contains("macro")
        );
    }
    let large = temp.0.join("large.docx");
    let file = std::fs::File::create(&large).unwrap();
    file.set_len(super::super::MAX_BYTES + 1).unwrap();
    assert!(
        super::super::file(&large, &temp.0.join("assets"))
            .unwrap_err()
            .to_string()
            .contains("250 MB")
    );
    let bytes = crate::deck::export::zip(vec![(
        "word/document.xml".into(),
        vec![b' '; 24 * 1024 * 1024 + 1],
    )])
    .unwrap();
    assert!(
        import(&bytes, &temp.0.join("assets"))
            .unwrap_err()
            .to_string()
            .contains("too large")
    );
    assert!(import(b"not a zip", &temp.0.join("assets")).is_err());
}
#[test]
fn dotx_and_relative_markdown_images_use_content_addressed_assets() {
    let temp = Temp::new();
    temp.image();
    let source = temp.0.join("notes.md");
    std::fs::write(
        &source,
        "# Report\n\n![Study](assets/sample.png \"Caption\")",
    )
    .unwrap();
    let imported = super::super::file(&source, &temp.0.join("destination/assets")).unwrap();
    let src = imported
        .doc
        .blocks
        .iter()
        .find_map(|b| {
            if let Kind::Image { src, .. } = &b.kind {
                Some(src)
            } else {
                None
            }
        })
        .unwrap();
    assert!(src.starts_with("assets/") && temp.0.join("destination").join(src).is_file());
    let path = temp.0.join("template.dotx");
    std::fs::write(
        &path,
        package("<w:p><w:r><w:t>Template text</w:t></w:r></w:p>", vec![]),
    )
    .unwrap();
    assert_eq!(
        super::super::file(&path, &temp.0.join("destination/assets"))
            .unwrap()
            .doc
            .blocks[0]
            .plain_text(),
        "Template text"
    );
}
