//! Bounded OPC copying and surgical XML edits; untouched parts stay byte-equal.
use std::{collections::BTreeMap, ops::Range};

use anyhow::{Context as _, Result, ensure};

use crate::deck::import::{pptx::package::Package, xml};

pub(super) type Parts = BTreeMap<String, Vec<u8>>;
pub(super) const OFFICE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

pub(super) fn open(bytes: &[u8]) -> Result<Parts> {
    ensure!(
        bytes.len() as u64 <= crate::deck::import::MAX_PPTX_BYTES,
        "template exceeds 250 MB"
    );
    let mut pkg = Package::open(bytes)?;
    let mut parts = Parts::new();
    for name in pkg.names() {
        ensure!(
            !name.starts_with('/') && !name.split('/').any(|s| s == "..") && !name.contains('\\'),
            "invalid template part name"
        );
        let lower = name.to_ascii_lowercase();
        ensure!(
            !lower.contains("vba") && !lower.contains("macros"),
            "macro templates are not supported"
        );
        let data = pkg.raw(&name)?;
        if name.ends_with(".xml") || name.ends_with(".rels") {
            xml::parse(std::str::from_utf8(&data)?)?;
        }
        parts.insert(name, data);
    }
    let types = text(&parts, "[Content_Types].xml")?;
    ensure!(
        !types.to_ascii_lowercase().contains("macroenabled") && !types.contains("vbaProject"),
        "macro templates are not supported"
    );
    Ok(parts)
}

pub(super) fn text<'a>(parts: &'a Parts, name: &str) -> Result<&'a str> {
    std::str::from_utf8(
        parts
            .get(name)
            .with_context(|| format!("missing template part {name}"))?,
    )
    .map_err(Into::into)
}

/// Byte ranges of complete elements, including self-closing ones. XML is already
/// checked by the bounded reader; quotes/comments/processing instructions are skipped.
pub(super) fn ranges(source: &str, local: &str) -> Vec<Range<usize>> {
    let mut stack = Vec::new();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = source[at..].find('<').map(|n| at + n) {
        let rest = &source[start..];
        let special = if rest.starts_with("<!--") {
            Some("-->")
        } else if rest.starts_with("<![CDATA[") {
            Some("]]>")
        } else if rest.starts_with("<?") {
            Some("?>")
        } else {
            None
        };
        if let Some(end) = special {
            at = start + rest.find(end).map_or(rest.len(), |n| n + end.len());
            continue;
        }
        let mut quote = None;
        let mut end = source.len();
        for (offset, ch) in source[start + 1..].char_indices() {
            match (quote, ch) {
                (None, '\'' | '"') => quote = Some(ch),
                (Some(q), c) if q == c => quote = None,
                (None, '>') => {
                    end = start + offset + 2;
                    break;
                }
                _ => {}
            }
        }
        let tag = source[start + 1..end - 1].trim();
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split([' ', '\t', '\r', '\n', '/'])
            .next()
            .unwrap_or("");
        if name.rsplit(':').next() == Some(local) {
            if closing {
                if let Some(begin) = stack.pop() {
                    out.push(begin..end);
                }
            } else if tag.ends_with('/') {
                out.push(start..end);
            } else {
                stack.push(start);
            }
        }
        at = end;
    }
    out.sort_by_key(|r| r.start);
    out
}

pub(super) fn replace(source: &str, local: &str, replacement: &str) -> String {
    let Some(range) = ranges(source, local).into_iter().next() else {
        return source.into();
    };
    format!(
        "{}{}{}",
        &source[..range.start],
        replacement,
        &source[range.end..]
    )
}

pub(super) fn append(source: &str, addition: &str) -> Result<String> {
    if let Some(at) = source.rfind("</") {
        return Ok(format!("{}{addition}{}", &source[..at], &source[at..]));
    }
    let root = xml::parse(source)?;
    let range = ranges(source, &root.name)
        .into_iter()
        .next()
        .context("XML root")?;
    let tag = &source[range.clone()];
    let open = tag
        .strip_suffix("/>")
        .context("XML root must have an end tag")?;
    let name = open[1..]
        .split_whitespace()
        .next()
        .context("XML root name")?;
    Ok(format!(
        "{}{open}>{addition}</{name}>{}",
        &source[..range.start],
        &source[range.end..]
    ))
}

pub(super) fn relationships(source: &str, keep: impl Fn(&xml::El) -> bool) -> Result<String> {
    let mut out = source.to_string();
    for range in ranges(source, "Relationship").into_iter().rev() {
        let rel = xml::parse(&source[range.clone()])?;
        if !keep(&rel) {
            out.replace_range(range, "");
        }
    }
    Ok(out)
}

pub(super) fn relationship(id: &str, kind: &str, target: &str) -> String {
    format!(
        "<Relationship Id=\"{}\" Type=\"{OFFICE}/{kind}\" Target=\"{}\"/>",
        crate::doc::inline::escape(id),
        crate::doc::inline::escape(target)
    )
}

/// Merge content types by part name, preserving template defaults and untouched
/// overrides. A generated override replaces only the corresponding written part.
pub(super) fn content_types(
    base: &str,
    generated: &str,
    parts: &Parts,
    written: &[String],
) -> Result<String> {
    let mut out = base.to_string();
    let mut additions = String::new();
    for range in ranges(base, "Override").into_iter().rev() {
        let el = xml::parse(&base[range.clone()])?;
        let name = el.attr("PartName").unwrap_or("").trim_start_matches('/');
        if !parts.contains_key(name) || written.iter().any(|s| s == name) {
            out.replace_range(range, "");
        }
    }
    for range in ranges(generated, "Override") {
        let el = xml::parse(&generated[range.clone()])?;
        let name = el.attr("PartName").unwrap_or("").trim_start_matches('/');
        if written.iter().any(|s| s == name) {
            additions.push_str(&generated[range]);
        }
    }
    let base_root = xml::parse(base)?;
    for range in ranges(generated, "Default") {
        let el = xml::parse(&generated[range.clone()])?;
        if !base_root
            .kids_named("Default")
            .any(|d| d.attr("Extension") == el.attr("Extension"))
        {
            additions.push_str(&generated[range]);
        }
    }
    append(&out, &additions)
}
