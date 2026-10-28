use super::*;
use crate::deck::{Font, Node, Position, Sizing};
use std::collections::BTreeMap;
use std::io::Read as _;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nexus-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn motion() -> Deck {
    Deck::check(include_str!(
        "../../../tests/fixtures/deck/export-motion.deck.json"
    ))
    .unwrap()
}

fn unzip(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    (0..zip.len())
        .map(|i| {
            let mut file = zip.by_index(i).unwrap();
            let name = file.name().to_string();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            (name, bytes)
        })
        .collect()
}

#[derive(Debug)]
struct Element {
    name: String,
    attrs: BTreeMap<String, String>,
}

// A small strict tag/attribute scanner, not a substring-balanced angle-bracket
// check: honours quoted '>', validates closing names, attributes and entities.
fn xml(source: &str) -> Vec<Element> {
    fn entities(text: &str) {
        for rest in text.split('&').skip(1) {
            let entity = rest.split_once(';').expect("terminated XML entity").0;
            assert!(
                matches!(entity, "amp" | "lt" | "gt" | "quot" | "apos")
                    || entity
                        .strip_prefix('#')
                        .is_some_and(|n| n.strip_prefix('x').map_or_else(
                            || n.parse::<u32>().is_ok(),
                            |n| u32::from_str_radix(n, 16).is_ok()
                        )),
                "{entity}"
            );
        }
    }
    let mut stack = Vec::new();
    let mut elements = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find('<') {
        entities(&rest[..at]);
        rest = &rest[at + 1..];
        let mut quote = None;
        let end = rest
            .char_indices()
            .find_map(|(i, c)| {
                if matches!(c, '\'' | '"') {
                    if quote == Some(c) {
                        quote = None;
                    } else if quote.is_none() {
                        quote = Some(c);
                    }
                }
                (c == '>' && quote.is_none()).then_some(i)
            })
            .expect("terminated XML tag");
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        if tag.starts_with('?') {
            assert!(tag.ends_with('?'));
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            assert_eq!(stack.pop().as_deref(), Some(name.trim()), "closing {name}");
            continue;
        }
        let empty = tag.ends_with('/');
        let tag = tag.trim_end_matches('/').trim();
        let end = tag.find(char::is_whitespace).unwrap_or(tag.len());
        let name = &tag[..end];
        assert!(!name.is_empty());
        let mut attributes = tag[end..].trim();
        let mut attrs = BTreeMap::new();
        while !attributes.is_empty() {
            let (key, after) = attributes.split_once('=').expect("XML attribute equals");
            let after = after.trim_start();
            let quote = after.chars().next().unwrap();
            assert!(matches!(quote, '\'' | '"'));
            let end = after[1..].find(quote).expect("quoted XML attribute") + 1;
            let value = &after[1..end];
            assert!(!value.contains('<'));
            entities(value);
            assert!(
                attrs
                    .insert(key.trim().to_string(), value.to_string())
                    .is_none()
            );
            attributes = after[end + 1..].trim();
        }
        if !empty {
            stack.push(name.to_string());
        }
        elements.push(Element {
            name: name.into(),
            attrs,
        });
    }
    assert!(stack.is_empty(), "unclosed XML {stack:?}");
    entities(rest);
    elements
}

fn package_valid(parts: &BTreeMap<String, Vec<u8>>) {
    let mut content = BTreeMap::new();
    for entry in xml(std::str::from_utf8(&parts["[Content_Types].xml"]).unwrap()) {
        if entry.name == "Override" {
            let name = entry.attrs["PartName"].trim_start_matches('/').to_string();
            assert!(parts.contains_key(&name), "missing content part {name}");
            content.insert(name, entry.attrs["ContentType"].clone());
        }
    }
    for (name, bytes) in parts {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let elements = xml(std::str::from_utf8(bytes).unwrap());
            if name != "[Content_Types].xml" && !name.ends_with(".rels") {
                assert!(content.contains_key(name), "unlisted part {name}");
            }
            if name.ends_with(".rels") {
                let directory = name.split_once("_rels/").unwrap().0;
                for element in elements.iter().filter(|e| e.name == "Relationship") {
                    let target = &element.attrs["Target"];
                    let mut path = directory
                        .split('/')
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>();
                    for component in target.split('/') {
                        if component == ".." {
                            path.pop().expect("relationship inside package");
                        } else if component != "." {
                            path.push(component);
                        }
                    }
                    let path = path.join("/");
                    assert!(
                        parts.contains_key(&path),
                        "{name}: missing rel target {path}"
                    );
                }
            }
        } else {
            assert!(content.contains_key(name), "unlisted media {name}");
        }
    }
}

#[test]
fn pptx_package_geometry_notes_text_timing_and_determinism() {
    let deck = motion();
    let output = export(&deck, None, Format::Pptx, Options::default()).unwrap();
    assert_eq!(output.bytes, pptx::export_pptx(&deck, None).unwrap());
    let parts = unzip(&output.bytes);
    package_valid(&parts);
    assert_eq!(
        parts
            .keys()
            .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
            .count(),
        2
    );
    assert_eq!(
        parts
            .keys()
            .filter(|n| n.starts_with("ppt/notesSlides/notesSlide") && n.ends_with(".xml"))
            .count(),
        2
    );
    assert_eq!(
        parts.keys().filter(|n| n.starts_with("ppt/media/")).count(),
        1
    );
    let slide = std::str::from_utf8(&parts["ppt/slides/slide1.xml"]).unwrap();
    let rects = crate::deck::layout_at(&deck, &deck.slides[0], None)
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let list = rects["list"];
    let scale = 12_192_000.0 / deck.theme.size.w;
    let geom = format!(
        "<a:off x=\"{:.0}\" y=\"{:.0}\"/><a:ext cx=\"{:.0}\" cy=\"{:.0}\"/>",
        list.x * scale,
        list.y * scale,
        list.w * scale,
        list.h * scale
    );
    assert!(slide.contains(&geom));
    assert!(!slide.contains("<p:grpSp>"), "frames must be flattened");
    for value in [
        "<p:ph type=\"title\"/>",
        "<a:buAutoNum",
        "<a:buChar",
        "<p:push dir=\"l\"/>",
        "<p:timing>",
        "build=\"p\"",
        "nodeType=\"withEffect\"",
        "nodeType=\"afterEffect\"",
        "<p:anim calcmode",
        "mc:AlternateContent",
        "p14:dur=\"600\"",
        "<p:animScale",
        "wipe(right)",
        "<p:pRg st=\"1\" end=\"1\"/>",
        "presetClass=\"exit\"",
        "ppt_x",
    ] {
        assert!(slide.contains(value), "{value}");
    }
    let second = std::str::from_utf8(&parts["ppt/slides/slide2.xml"]).unwrap();
    assert!(second.contains("<p159:morph option=\"byObject\"/>"));
    assert!(second.contains("<mc:Fallback><p:transition spd=\"med\"><p:fade/>"));
    assert!(!second.contains("<p:timing>"));
    assert!(
        std::str::from_utf8(&parts["ppt/notesSlides/notesSlide1.xml"])
            .unwrap()
            .contains("Pause here.")
    );
    assert!(
        output
            .warnings
            .iter()
            .any(|w| w.contains("smart transition"))
    );
    let temp = Temp::new();
    let path = temp.0.join("deck.pptx");
    std::fs::write(&path, &output.bytes).unwrap();
    let extracted = crate::extract::to_text(&path).text;
    assert_eq!(
        extracted,
        "[slide 1]\nA clearer story\nFind \nsignal\nKeep \ncontext\nUse \nevidence\nShare clarity\n[slide 2]\nEvidence\nPhase\nValue\nStart\n3\nGrow\n8\nlet evidence = 8;"
    );
}

#[test]
fn pptx_images_crops_fonts_and_missing_assets_are_offline() {
    let temp = Temp::new();
    let image = image::RgbImage::from_pixel(200, 100, image::Rgb([200, 50, 20]));
    image.save(temp.0.join("photo.png")).unwrap();
    let bytes = std::fs::read(temp.0.join("photo.png")).unwrap();
    crate::remote_image::Fetcher::new(&temp.0)
        .seed("https://example.invalid/photo", &bytes)
        .unwrap();
    let mut deck = Deck::titled("Images");
    for (id, src, fit) in [
        ("cover", "assets/photo.png", crate::deck::Fit::Cover),
        (
            "contain",
            "https://example.invalid/photo",
            crate::deck::Fit::Contain,
        ),
        (
            "missing",
            "https://example.invalid/missing",
            crate::deck::Fit::Contain,
        ),
    ] {
        let mut node = Node::new(
            id,
            Kind::Image {
                src: src.into(),
                fit,
            },
        );
        node.sizing = Sizing::fixed(100.0, 100.0);
        node.position = Position::Absolute { x: 100.0, y: 100.0 };
        deck.slides[0].children.push(node);
    }
    deck.theme.font_files.push(crate::deck::FontFile {
        family: "Imported".into(),
        file: "assets/fonts/imported.ttf".into(),
        weight: 400,
        italic: false,
        fallback: Vec::new(),
    });
    let output = export(&deck, Some(&temp.0), Format::Pptx, Options::default()).unwrap();
    let parts = unzip(&output.bytes);
    package_valid(&parts);
    let slide = std::str::from_utf8(&parts["ppt/slides/slide1.xml"]).unwrap();
    assert!(slide.contains("<a:srcRect l=\"25000\" r=\"25000\""));
    assert!(slide.contains("Image not available"));
    assert_eq!(
        parts.keys().filter(|n| n.starts_with("ppt/media/")).count(),
        2
    );
    assert!(
        output
            .warnings
            .iter()
            .any(|w| w.contains("Install font 'Imported'"))
    );
    assert!(output.warnings.iter().any(|w| w.contains("missing")));
    let html = export(&deck, Some(&temp.0), Format::Html, Options::default()).unwrap();
    let html = String::from_utf8(html.bytes).unwrap();
    assert!(html.contains("data:image/png;base64,"));
    assert!(!html.contains("https://example.invalid/photo"));
}

fn has_fonts() -> bool {
    crate::deck::text::system_fonts().faces().next().is_some()
}

#[test]
fn pdf_is_vector_with_real_text_unicode_and_determinism() {
    let mut deck = motion();
    deck.slides[1].children.push(Node::new(
        "unicode",
        Kind::Text {
            text: "Café résumé".into(),
            style: "caption".into(),
            font: Font::default(),
        },
    ));
    let bytes = pdf::export_pdf(&deck, None).unwrap();
    assert_eq!(bytes, pdf::export_pdf(&deck, None).unwrap());
    let doc = pdf_extract::Document::load_mem(&bytes).unwrap();
    assert_eq!(doc.get_pages().len(), 2);
    for page in doc.get_pages().values() {
        let dict = doc.get_object(*page).unwrap().as_dict().unwrap();
        let media = dict.get(b"MediaBox").unwrap().as_array().unwrap();
        assert_eq!(media[2].as_float().unwrap(), 960.0);
        assert_eq!(media[3].as_float().unwrap(), 540.0);
    }
    // Text slides are vector: no image XObject covers a page, and the old
    // invisible-text rendering mode is gone.
    for object in doc.objects.values() {
        if let Ok(stream) = object.as_stream() {
            assert_ne!(
                stream
                    .dict
                    .get(b"Subtype")
                    .ok()
                    .and_then(|v| v.as_name().ok()),
                Some(b"Image".as_slice())
            );
        }
    }
    for page in doc.get_pages().values() {
        let content = String::from_utf8_lossy(&doc.get_page_content(*page).unwrap()).into_owned();
        assert!(!content.contains("3 Tr"));
    }
    assert!(
        bytes.len() < 400_000,
        "vector pages stay small: {}",
        bytes.len()
    );
    if !has_fonts() {
        return;
    }
    // Glyphs are positioned one by one, so extractors may add spaces.
    let text = pdf_extract::extract_text_from_mem(&bytes)
        .unwrap()
        .split_whitespace()
        .collect::<String>();
    for needle in [
        "Aclearerstory",
        "signal",
        "context",
        "evidence",
        "Evidence",
        "Phase",
        "Caféré",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
    assert!(!text.contains("Pausehere"));
    // The code slide's `=` is real text too: every font carries a ToUnicode
    // map, and one of them maps a glyph to U+003D. (Extractors garble
    // individually positioned monospace glyphs, so read the map instead.)
    let maps = doc
        .objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter(|d| {
            d.get(b"Subtype").ok().and_then(|t| t.as_name().ok()) == Some(b"Type0".as_slice())
        })
        .map(|d| {
            let id = d.get(b"ToUnicode").unwrap().as_reference().unwrap();
            let stream = doc.get_object(id).unwrap().as_stream().unwrap();
            String::from_utf8_lossy(&stream.decompressed_content().unwrap()).into_owned()
        })
        .collect::<Vec<_>>();
    assert!(maps.iter().any(|m| m.contains("> <003D>")), "{maps:?}");
}

#[test]
fn png_zip_size_and_html_single_file_contains_every_slide_without_network_or_notes() {
    let deck = motion();
    let zip = images::export_png(&deck, None, 320).unwrap();
    let parts = unzip(&zip);
    assert_eq!(parts.len(), 2);
    for (i, bytes) in parts.values().enumerate() {
        assert!(parts.contains_key(&format!("slide-{:03}.png", i + 1)));
        let image = image::load_from_memory(bytes).unwrap();
        assert_eq!((image.width(), image.height()), (320, 180));
    }
    assert!(image::load_from_memory(&images::contact_sheet(&deck, None).unwrap()).is_ok());
    let html = html::export_html(&deck, None).unwrap();
    for slide in &deck.slides {
        assert!(html.contains(&format!("\"id\":\"{}\"", slide.id)));
    }
    assert!(html.contains("A clearer story"));
    assert!(!html.contains("Pause here"));
    for external in [
        "src=\"http",
        "href=\"http",
        "src=\"//",
        "href=\"//",
        "import ",
        "src=\"/app",
        "href=\"/studio",
    ] {
        assert!(!html.contains(external), "{external}");
    }
    for code in [
        "function playbackController",
        "function transitionSlides",
        "function renderSlide",
        "ArrowRight",
        "default-src 'none'",
    ] {
        assert!(html.contains(code));
    }
    assert_eq!(html, html::export_html(&deck, None).unwrap());
}

#[test]
fn destinations_are_confined_unique_and_explicit_paths_do_not_overwrite() {
    let temp = Temp::new();
    let output = export(
        &Deck::titled("../A story!"),
        None,
        Format::Json,
        Options::default(),
    )
    .unwrap();
    let one = write_export(&output, &temp.0, None).unwrap();
    let two = write_export(&output, &temp.0, None).unwrap();
    assert_eq!(one, temp.0.join("exports/a-story.deck.json"));
    assert_eq!(two, temp.0.join("exports/a-story (2).deck.json"));
    let custom = temp.0.join("custom.html");
    std::fs::write(&custom, b"keep me").unwrap();
    assert_eq!(
        write_export(&output, &temp.0, Some(&custom)).unwrap(),
        temp.0.join("custom (2).html")
    );
    assert_eq!(std::fs::read(&custom).unwrap(), b"keep me");
    assert!(write_export(&output, &temp.0, Some(Path::new("relative.json"))).is_err());
    assert!(write_export(&output, &temp.0, Some(&temp.0.join("../escape.json"))).is_err());
}

#[cfg(unix)]
#[test]
fn export_refuses_symlink_destinations() {
    let temp = Temp::new();
    let outside = Temp::new();
    std::os::unix::fs::symlink(&outside.0, temp.0.join("exports")).unwrap();
    let output = export(
        &Deck::titled("Title"),
        None,
        Format::Json,
        Options::default(),
    )
    .unwrap();
    assert!(write_export(&output, &temp.0, None).is_err());
    assert_eq!(std::fs::read_dir(&outside.0).unwrap().count(), 0);
}

#[test]
fn write_reviewer_export_samples_when_requested() {
    let Some(out) = std::env::var_os("NEXUS_EXPORT_OUT") else {
        return;
    };
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    for (name, deck) in [
        ("classic", Deck::classic_fixture("Classic fixture")),
        ("auto-motion", motion()),
    ] {
        let mut notes = String::new();
        for format in [Format::Pptx, Format::Pdf, Format::Png, Format::Html] {
            let output = export(&deck, None, format, Options::default()).unwrap();
            std::fs::write(
                out.join(format!("{name}.{}", format.extension())),
                &output.bytes,
            )
            .unwrap();
            notes.push_str(&format!("{format:?}:\n{}\n", output.warnings.join("\n")));
        }
        std::fs::write(out.join(format!("{name}-warnings.txt")), notes).unwrap();
    }
}
