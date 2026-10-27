//! Word package reuse: names resolve localized ids; only missing, used styles are added.
use super::package::{self, Parts};
use crate::deck::import::xml::{self, El};
use anyhow::{Context as _, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

fn val<'a>(el: &'a El, child: &str) -> Option<&'a str> {
    el.kid(child)?.attr_local("val")
}
fn change_values(source: &str, element: &str, ids: &BTreeMap<String, String>) -> Result<String> {
    let mut out = source.to_string();
    for range in package::ranges(source, element).into_iter().rev() {
        let raw = &source[range.clone()];
        let el = xml::parse(raw)?;
        if let Some(old) = el.attr_local("val")
            && let Some(new) = ids.get(old)
        {
            let escaped = crate::doc::inline::escape(new);
            out.replace_range(
                range,
                &raw.replace(&format!("w:val=\"{old}\""), &format!("w:val=\"{escaped}\"")),
            );
        }
    }
    Ok(out)
}

fn styles(
    base: &mut Parts,
    generated: &Parts,
    document: &mut String,
) -> Result<BTreeMap<String, String>> {
    let original = base.get("word/styles.xml").map(|b| std::str::from_utf8(b)).transpose()?.unwrap_or("<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"></w:styles>").to_string();
    let template = xml::parse(&original)?;
    let generated_xml = package::text(generated, "word/styles.xml")?;
    let mut names = BTreeMap::new();
    let mut occupied = BTreeSet::new();
    for style in template.kids_named("style") {
        if let (Some(id), Some(name)) = (style.attr_local("styleId"), val(style, "name")) {
            if name.eq_ignore_ascii_case("code") {
                names.insert("code".into(), id.to_string());
            } else {
                names
                    .entry(name.to_ascii_lowercase())
                    .or_insert_with(|| id.to_string());
            }
            occupied.insert(id.to_string());
            if super::word::role(name) == Some("code")
                || style
                    .find("rFonts")
                    .and_then(|f| f.attr_local("ascii"))
                    .is_some_and(super::word::monospace)
            {
                names.entry("code".into()).or_insert_with(|| id.to_string());
            }
        }
    }
    let mut ids = BTreeMap::new();
    let mut candidates = BTreeMap::new();
    for range in package::ranges(generated_xml, "style") {
        let raw = &generated_xml[range];
        let el = xml::parse(raw)?;
        let id = el.attr_local("styleId").context("generated style id")?;
        let name = val(&el, "name").unwrap_or(id).to_ascii_lowercase();
        let resolved = if let Some(found) = names.get(&name) {
            found.clone()
        } else {
            let mut candidate = id.to_string();
            while occupied.contains(&candidate) || ids.values().any(|v| v == &candidate) {
                candidate = format!("Nexus{candidate}");
            }
            candidate
        };
        ids.insert(id.into(), resolved);
        candidates.insert(id.to_string(), raw.to_string());
    }
    let mut used = BTreeSet::new();
    for source in std::iter::once(document.as_str()).chain(
        ["word/footnotes.xml", "word/comments.xml"]
            .into_iter()
            .filter_map(|n| package::text(generated, n).ok()),
    ) {
        let root = xml::parse(source)?;
        for element in ["pStyle", "rStyle"] {
            let mut refs = Vec::new();
            root.descendants(element, &mut refs);
            for reference in refs {
                if let Some(id) = reference.attr_local("val") {
                    used.insert(id.to_string());
                }
            }
        }
    }
    let mut additions = String::new();
    let mut done = BTreeSet::new();
    while let Some(id) = used.pop_first() {
        if !done.insert(id.clone()) {
            continue;
        }
        let Some(raw) = candidates.get(&id) else {
            continue;
        };
        let resolved = &ids[&id];
        if occupied.contains(resolved) {
            continue;
        }
        let el = xml::parse(raw)?;
        for key in ["basedOn", "next", "link"] {
            if let Some(dependency) = val(&el, key) {
                used.insert(dependency.into());
            }
        }
        let mut added = raw.replace(
            &format!("w:styleId=\"{id}\""),
            &format!("w:styleId=\"{}\"", crate::doc::inline::escape(resolved)),
        );
        for key in ["basedOn", "next", "link"] {
            added = change_values(&added, key, &ids)?;
        }
        additions.push_str(&added);
        occupied.insert(resolved.clone());
    }
    if !additions.is_empty() {
        base.insert(
            "word/styles.xml".into(),
            package::append(&original, &additions)?.into_bytes(),
        );
    }
    *document = change_values(document, "pStyle", &ids)?;
    *document = change_values(document, "rStyle", &ids)?;
    Ok(ids)
}

fn numbering(base: &mut Parts, generated: &Parts, document: &mut String) -> Result<()> {
    let generated_xml = package::text(generated, "word/numbering.xml")?;
    let original = base.get("word/numbering.xml").map(|b| std::str::from_utf8(b)).transpose()?.unwrap_or("<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"></w:numbering>").to_string();
    let template = xml::parse(&original)?;
    let mut abstract_ids = BTreeMap::new();
    let mut next_abstract = 0_u64;
    for el in template.kids_named("abstractNum") {
        let id = el.attr_local("abstractNumId").unwrap_or("0");
        next_abstract = next_abstract.max(id.parse::<u64>().unwrap_or(0) + 1);
        if let Some(format) = el.kid("lvl").and_then(|lvl| val(lvl, "numFmt")) {
            abstract_ids
                .entry(format.to_string())
                .or_insert_with(|| id.to_string());
        }
    }
    let mut nums = BTreeMap::new();
    let mut next_num = 1_u64;
    for el in template.kids_named("num") {
        let id = el.attr_local("numId").unwrap_or("0");
        next_num = next_num.max(id.parse::<u64>().unwrap_or(0) + 1);
        if let Some(abstract_id) = val(el, "abstractNumId") {
            nums.entry(abstract_id.to_string())
                .or_insert_with(|| id.to_string());
        }
    }
    let generated_root = xml::parse(generated_xml)?;
    let mut id_map = BTreeMap::new();
    let mut added_abstracts = String::new();
    let mut added_nums = String::new();
    let root = xml::parse(document)?;
    let mut used = Vec::new();
    root.descendants("numId", &mut used);
    for reference in used {
        let Some(id) = reference.attr_local("val") else {
            continue;
        };
        if id_map.contains_key(id) {
            continue;
        }
        let num = generated_root
            .kids_named("num")
            .find(|n| n.attr_local("numId") == Some(id))
            .context("generated numbering instance")?;
        let abstract_id = val(num, "abstractNumId").context("generated abstract numbering id")?;
        let abstract_num = generated_root
            .kids_named("abstractNum")
            .find(|n| n.attr_local("abstractNumId") == Some(abstract_id))
            .context("generated abstract numbering")?;
        let format = abstract_num
            .kid("lvl")
            .and_then(|n| val(n, "numFmt"))
            .unwrap_or("bullet");
        let target = if let Some(existing) = abstract_ids.get(format) {
            existing.clone()
        } else {
            let target = next_abstract.to_string();
            next_abstract += 1;
            let raw = package::ranges(generated_xml, "abstractNum")
                .into_iter()
                .map(|r| &generated_xml[r])
                .find(|s| {
                    xml::parse(s).is_ok_and(|n| n.attr_local("abstractNumId") == Some(abstract_id))
                })
                .context("numbering XML")?;
            added_abstracts.push_str(&raw.replace(
                &format!("w:abstractNumId=\"{abstract_id}\""),
                &format!("w:abstractNumId=\"{target}\""),
            ));
            abstract_ids.insert(format.into(), target.clone());
            target
        };
        let number = nums.entry(target.clone()).or_insert_with(|| {
            let number = next_num.to_string();
            next_num += 1;
            let _ = write!(
                added_nums,
                "<w:num w:numId=\"{number}\"><w:abstractNumId w:val=\"{target}\"/></w:num>"
            );
            number
        });
        id_map.insert(id.into(), number.clone());
    }
    if !added_abstracts.is_empty()
        || !added_nums.is_empty()
        || !base.contains_key("word/numbering.xml")
    {
        let mut merged = original;
        if let Some(first) = package::ranges(&merged, "num").first() {
            merged.insert_str(first.start, &added_abstracts);
        } else {
            merged = package::append(&merged, &added_abstracts)?;
        }
        merged = package::append(&merged, &added_nums)?;
        base.insert("word/numbering.xml".into(), merged.into_bytes());
    }
    *document = change_values(document, "numId", &id_map)?;
    Ok(())
}

fn namespaces(document: &str, original: &str) -> String {
    let Some(root) = package::ranges(original, "document").into_iter().next() else {
        return document.into();
    };
    let tag = &original[root.start
        ..original[root.start..]
            .find('>')
            .map_or(root.end, |n| root.start + n)];
    let mut out = document.to_string();
    let mut rest = tag;
    while let Some(at) = rest.find("xmlns:") {
        rest = &rest[at..];
        let Some(eq) = rest.find('=') else {
            break;
        };
        let key = &rest[..eq];
        let after = rest[eq + 1..].trim_start();
        let quote = after.chars().next().unwrap_or('"');
        let Some(end) = after[1..].find(quote) else {
            break;
        };
        let value = &after[1..=end];
        if !out.contains(&format!("{key}=")) {
            out = out.replacen(
                "<w:document ",
                &format!("<w:document {key}=\"{value}\" "),
                1,
            );
        }
        rest = &after[end + 2..];
    }
    out
}

#[allow(clippy::too_many_lines)] // One ordered OPC package rewrite.
pub(super) fn merge(template: &[u8], generated: &[u8]) -> Result<Vec<u8>> {
    let mut base = package::open(template)?;
    let mut generated = package::open(generated)?;
    let original_types = package::text(&base, "[Content_Types].xml")?.to_string();
    let mut types = package::text(&generated, "[Content_Types].xml")?.to_string();
    let original_document = package::text(&base, "word/document.xml")?.to_string();
    let mut document = package::text(&generated, "word/document.xml")?.to_string();
    let ids = styles(&mut base, &generated, &mut document)?;
    numbering(&mut base, &generated, &mut document)?;
    let empty = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>";
    let mut rels = package::relationships(
        base.get("word/_rels/document.xml.rels")
            .map(|s| std::str::from_utf8(s))
            .transpose()?
            .unwrap_or(empty),
        |r| {
            !["/image", "/hyperlink", "/footnotes", "/comments"]
                .iter()
                .any(|k| r.attr("Type").unwrap_or("").ends_with(k))
        },
    )?;
    let old_rels = xml::parse(&rels)?;
    let generated_rels = package::text(&generated, "word/_rels/document.xml.rels")?.to_string();
    let mut rel_ids = BTreeMap::new();
    let mut media_ids = BTreeMap::new();
    for range in package::ranges(&generated_rels, "Relationship") {
        let raw = &generated_rels[range];
        let el = xml::parse(raw)?;
        let id = el.attr("Id").context("generated relationship id")?;
        let kind = el.attr("Type").unwrap_or("");
        if ["/header", "/footer"].iter().any(|k| kind.ends_with(k)) {
            continue;
        }
        if ["/styles", "/theme", "/numbering"]
            .iter()
            .any(|k| kind.ends_with(k))
            && old_rels
                .kids_named("Relationship")
                .any(|r| r.attr("Type") == Some(kind))
        {
            continue;
        }
        let mut new_id = format!("nexus{id}");
        while old_rels
            .kids_named("Relationship")
            .any(|r| r.attr("Id") == Some(&new_id))
        {
            new_id.push('x');
        }
        rel_ids.insert(id.to_string(), new_id.clone());
        let mut added = raw.replace(&format!("Id=\"{id}\""), &format!("Id=\"{new_id}\""));
        if kind.ends_with("/image")
            && let Some(target) = el.attr("Target")
        {
            let mut new_target = format!("media/nexus-{}", target.trim_start_matches("media/"));
            while base.contains_key(&format!("word/{new_target}"))
                || media_ids
                    .values()
                    .any(|v| v == &format!("word/{new_target}"))
            {
                new_target = new_target.replacen("media/", "media/nexus-", 1);
            }
            media_ids.insert(format!("word/{target}"), format!("word/{new_target}"));
            added = added.replace(
                &format!("Target=\"{target}\""),
                &format!("Target=\"{new_target}\""),
            );
        }
        rels = package::append(&rels, &added)?;
    }
    for (old, new) in &rel_ids {
        for attr in ["id", "embed", "link"] {
            document = document.replace(
                &format!("r:{attr}=\"{old}\""),
                &format!("r:{attr}=\"{new}\""),
            );
        }
    }
    // Template section relationship ids belong to the retained relationships,
    // not the generated document's id namespace.
    if let Some(section) = package::ranges(&original_document, "sectPr").last() {
        document = package::replace(&document, "sectPr", &original_document[section.clone()]);
    }
    document = namespaces(&document, &original_document);
    let mut written = vec!["word/document.xml".to_string()];
    base.insert("word/document.xml".into(), document.into_bytes());
    base.insert("word/_rels/document.xml.rels".into(), rels.into_bytes());
    for (old, new) in media_ids {
        if let Some(bytes) = generated.remove(&old) {
            base.insert(new.clone(), bytes);
            written.push(new.clone());
            types = types.replace(&format!("/{old}\""), &format!("/{new}\""));
        }
    }
    for name in [
        "word/footnotes.xml",
        "word/comments.xml",
        "word/_rels/footnotes.xml.rels",
    ] {
        if let Some(bytes) = generated.get(name) {
            let mut source = std::str::from_utf8(bytes)?.to_string();
            for element in ["pStyle", "rStyle"] {
                source = change_values(&source, element, &ids)?;
            }
            if name.ends_with(".xml") {
                for (old, new) in &rel_ids {
                    source = source.replace(&format!("r:id=\"{old}\""), &format!("r:id=\"{new}\""));
                }
            } else {
                for (old, new) in &rel_ids {
                    source = source.replace(&format!("Id=\"{old}\""), &format!("Id=\"{new}\""));
                }
            }
            base.insert(name.into(), source.into_bytes());
            written.push(name.into());
        }
    }
    for name in [
        "word/styles.xml",
        "word/numbering.xml",
        "word/theme/theme1.xml",
    ] {
        if !base.contains_key(name)
            && let Some(bytes) = generated.get(name)
        {
            base.insert(name.into(), bytes.clone());
            written.push(name.into());
        }
        // Numbering and fallback styles added above need a content type if the
        // template originally had no corresponding override.
        if !original_types.contains(&format!("/{name}\"")) && base.contains_key(name) {
            written.push(name.into());
        }
    }
    let types = package::content_types(&original_types, &types, &base, &written)?.replace(
        "wordprocessingml.template.main+xml",
        "wordprocessingml.document.main+xml",
    );
    base.insert("[Content_Types].xml".into(), types.into_bytes());
    crate::deck::export::zip(base.into_iter().collect())
}
