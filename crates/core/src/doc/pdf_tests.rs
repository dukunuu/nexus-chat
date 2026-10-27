use super::*;
use test_support::{Temp, sample};

fn fonts() -> bool {
    crate::deck::text::system_fonts().faces().next().is_some()
}
fn squash(text: &str) -> String {
    text.split_whitespace().collect()
}
fn pdf(doc: &Doc, temp: &Temp) -> Vec<u8> {
    export::pdf::export(doc, Some(&temp.0.join("assets"))).unwrap()
}

#[test]
fn pdf_has_one_sized_page_per_layout_page_and_pinned_metadata() {
    let temp = Temp::new();
    temp.image();
    let doc = sample();
    let bytes = pdf(&doc, &temp);
    if std::env::var_os("NEXUS_DOC_PREVIEWS").is_some() {
        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/doc-previews");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("sample.pdf"), &bytes).unwrap();
    }
    assert!(bytes.starts_with(b"%PDF-"));
    let parsed = pdf_extract::Document::load_mem(&bytes).unwrap();
    let pages = layout::paginate(&doc, Some(&temp.0.join("assets")));
    assert_eq!(parsed.get_pages().len(), pages.len());
    assert!(pages.len() >= 2, "the sample has a page break");
    for (number, id) in &parsed.get_pages() {
        let page = &pages[*number as usize - 1];
        let dict = parsed.get_object(*id).unwrap().as_dict().unwrap();
        let media = dict.get(b"MediaBox").unwrap().as_array().unwrap();
        assert!((f64::from(media[2].as_float().unwrap()) - page.width).abs() < 0.01);
        assert!((f64::from(media[3].as_float().unwrap()) - page.height).abs() < 0.01);
    }
    let info = parsed
        .trailer
        .get(b"Info")
        .and_then(|i| i.as_reference())
        .and_then(|r| parsed.get_object(r))
        .and_then(|o| o.as_dict())
        .unwrap();
    let title = pdf_extract::decode_text_string(info.get(b"Title").unwrap()).unwrap();
    assert_eq!(title, "Research report");
    assert!(info.get(b"CreationDate").is_err(), "no clock in the bytes");
}

#[test]
fn pdf_text_is_real_selectable_text_with_the_doc_words() {
    if !fonts() {
        return;
    }
    let temp = Temp::new();
    temp.image();
    let bytes = pdf(&sample(), &temp);
    let text = squash(&pdf_extract::extract_text_from_mem(&bytes).unwrap());
    for needle in [
        "Researchreport",
        "Findings",
        "Reliableevidenceshouldbelegible",
        "Clarityispartofcorrectness",
        "select region, revenue".replace(' ', "").as_str(),
        "Nextsteps",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
    // Text pages carry fonts with a ToUnicode map, not page-sized pictures.
    let parsed = pdf_extract::Document::load_mem(&bytes).unwrap();
    let fonts = parsed
        .objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter(|d| {
            d.get(b"Subtype").ok().and_then(|t| t.as_name().ok()) == Some(b"Type0".as_slice())
        })
        .collect::<Vec<_>>();
    assert!(!fonts.is_empty());
    assert!(fonts.iter().all(|f| f.get(b"ToUnicode").is_ok()));
}

#[test]
fn pdf_has_heading_outline_and_link_annotations() {
    if !fonts() {
        return;
    }
    let temp = Temp::new();
    temp.image();
    let bytes = pdf(&sample(), &temp);
    let parsed = pdf_extract::Document::load_mem(&bytes).unwrap();
    let toc = parsed.get_toc().unwrap();
    let titles = toc.toc.iter().map(|e| e.title.as_str()).collect::<Vec<_>>();
    assert_eq!(titles, ["Findings", "Next steps"]);
    assert!(toc.toc[0].page >= 1 && toc.toc[1].page > toc.toc[0].page);
    let mut uris = Vec::new();
    for id in parsed.get_pages().values() {
        for annotation in parsed.get_page_annotations(*id).unwrap() {
            if annotation.get(b"Subtype").unwrap().as_name().unwrap() == b"Link" {
                let action = annotation.get(b"A").unwrap().as_dict().unwrap();
                uris.push(
                    String::from_utf8_lossy(action.get(b"URI").unwrap().as_str().unwrap())
                        .into_owned(),
                );
            }
        }
    }
    assert_eq!(uris, ["https://example.test/report"]);
}

#[test]
fn pdf_bytes_are_deterministic() {
    let temp = Temp::new();
    temp.image();
    let doc = sample();
    assert_eq!(pdf(&doc, &temp), pdf(&doc, &temp));
}
