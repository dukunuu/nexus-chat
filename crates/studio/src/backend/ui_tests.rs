//! The doc editor's UI modules: embedded and served, free of HTML injection, kept to the
//! core's spacing numbers, and (when `node` is installed) tested as pure functions.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::embedded;

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ui")
}

/// `node` 22 or newer, which imports the UI's ES modules directly. Absent on some CI
/// images; the tests that need it then skip with a note rather than fail.
fn node() -> Option<String> {
    let output = Command::new("node").arg("--version").output().ok()?;
    let version = String::from_utf8(output.stdout).ok()?;
    let major: u32 = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()?;
    (major >= 22).then(|| "node".into())
}

fn run_unit(args: &[&str]) -> Option<std::process::Output> {
    let node = node().or_else(|| {
        eprintln!("skipped: node 22+ is not installed");
        None
    })?;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui-dev/doc-unit.mjs");
    Some(
        Command::new(node)
            .arg(script)
            .args(args)
            .output()
            .expect("run node"),
    )
}

#[test]
fn every_doc_module_is_embedded_and_every_import_resolves() {
    let dir = ui_dir().join("doc");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (mime, served) = embedded(&format!("/doc/{name}"))
            .unwrap_or_else(|| panic!("/doc/{name} is not embedded"));
        assert_eq!(served, std::fs::read_to_string(&path).unwrap(), "{name}");
        assert!(
            (name.ends_with(".js") && mime == "text/javascript")
                || (name.ends_with(".css") && mime.starts_with("text/css")),
            "{name}: {mime}"
        );
        seen += 1;
        if !name.ends_with(".js") {
            continue;
        }
        for line in served
            .lines()
            .filter(|l| l.trim_start().starts_with("import "))
        {
            let Some(target) = line.split(['\'', '"']).nth(1) else {
                continue;
            };
            let resolved = if let Some(rest) = target.strip_prefix("./") {
                format!("/doc/{rest}")
            } else if let Some(rest) = target.strip_prefix("../") {
                format!("/{rest}")
            } else {
                panic!("{name}: unexpected import {target}");
            };
            assert!(embedded(&resolved).is_some(), "{name} imports {target}");
        }
    }
    assert!(seen >= 12, "the doc UI has its modules and stylesheet");
    // The shell page links the stylesheet and holds the mount point, before the
    // presentation markup the HTML export embeds.
    let (_, index) = embedded("/index.html").unwrap();
    assert!(index.contains("href=\"/doc/doc.css\""));
    assert!(
        index.find("id=\"doc-app\"").unwrap() < index.find("id=\"presentation\"").unwrap(),
        "the doc mount stays out of the exported presentation"
    );
}

#[test]
fn the_doc_ui_builds_the_page_without_markup_injection() {
    // Block text, comments and pasted content only ever become text nodes.
    for entry in std::fs::read_dir(ui_dir().join("doc")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "js") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        for forbidden in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function",
            "fetch(",
            "XMLHttpRequest",
            "WebSocket",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} uses {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn doc_css_uses_the_core_layout_numbers() {
    use nexus_core::doc::LAYOUT;
    let css = std::fs::read_to_string(ui_dir().join("doc/doc.css")).unwrap();
    let n = |value: f64| format!("{value}");
    for (name, value) in [
        ("block-gap", LAYOUT.block_gap),
        ("heading-above", LAYOUT.heading_above),
        ("heading-below", LAYOUT.heading_below),
        ("list-indent", LAYOUT.list_indent),
        ("quote-padding", LAYOUT.quote_padding),
        ("callout-padding", LAYOUT.callout_padding),
        ("cell-padding", LAYOUT.cell_padding),
        ("code-padding", LAYOUT.code_padding),
        ("caption-gap", LAYOUT.caption_gap),
        ("list-gap", LAYOUT.list_gap),
        ("bar-width", LAYOUT.bar_width),
    ] {
        let needle = format!("--l-{name}:{}pt;", n(value));
        assert!(css.contains(&needle), "doc.css is missing `{needle}`");
    }
    // Every spacing in the page comes from those variables, scaled by the screen zoom.
    for use_ in [
        "margin:0 0 calc(var(--l-block-gap) * var(--z))",
        "margin-top:calc(var(--l-heading-above) * var(--z))",
        "margin-bottom:calc(var(--l-heading-below) * var(--z))",
        "padding-left:calc(var(--l-list-indent) * var(--z))",
        "padding:calc(var(--l-cell-padding) * var(--z))",
        "padding:calc(var(--l-code-padding) * var(--z))",
        "margin-top:calc(var(--l-caption-gap) * var(--z))",
        "margin-top:calc(var(--l-list-gap) * var(--z))",
        "border-left:calc(var(--l-bar-width) * var(--z))",
    ] {
        assert!(css.contains(use_), "doc.css no longer uses `{use_}`");
    }
}

#[test]
fn inline_markdown_and_the_structural_edits_pass_their_unit_tests() {
    let Some(output) = run_unit(&[]) else { return };
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("passed"), "{text}");
}

#[test]
fn what_the_editor_writes_reads_back_the_same_through_the_core_parser() {
    let Some(output) = run_unit(&["--parity"]) else {
        return;
    };
    assert!(output.status.success());
    let pairs: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(pairs.len() > 250);
    for pair in pairs {
        let (md, plain) = (
            pair["md"].as_str().unwrap(),
            pair["plain"].as_str().unwrap(),
        );
        assert_eq!(
            nexus_core::doc::markdown::plain(md),
            plain,
            "the editor's serialization {md:?} reads differently in the core"
        );
    }
}
