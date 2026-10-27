//! Located document validation and stable-id repair.
use super::model::{Block, Doc, Kind, ListItem, Page, Paper, Theme};
use crate::deck::{CheckError, Deck};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn issue(errors: &mut Vec<CheckError>, path: impl Into<String>, message: impl Into<String>) {
    errors.push(CheckError {
        path: path.into(),
        message: message.into(),
    });
}

/// Check shared tokens using the deck validator, keeping document paths.
pub(crate) fn check_theme(theme: &Theme) -> Vec<CheckError> {
    let slide_theme = crate::deck::Theme {
        colors: theme.colors.clone(),
        fonts: theme.fonts.clone(),
        font_files: theme.font_files.clone(),
        text_styles: theme.text_styles.clone(),
        ..Default::default()
    };
    let mut errors = Deck::check(
        &json!({"version": 2, "title": "Theme", "theme": slide_theme, "slides": []}).to_string(),
    )
    .err()
    .unwrap_or_default();
    if !(20..=200).contains(&theme.measure_ch) {
        issue(
            &mut errors,
            "$.theme.measure_ch",
            "expected 20 to 200 characters",
        );
    }
    errors
}

/// Check margins against physical paper dimensions.
pub(crate) fn check_page(page: &Page) -> Vec<CheckError> {
    let mut errors = Vec::new();
    let (w, h) = match page.size {
        Paper::A4 => (595.28, 841.89),
        Paper::Letter => (612.0, 792.0),
    };
    if page.margins_pt.iter().any(|n| !n.is_finite() || *n < 0.0)
        || page.margins_pt[0] + page.margins_pt[2] >= h
        || page.margins_pt[1] + page.margins_pt[3] >= w
    {
        issue(
            &mut errors,
            "$.page.margins_pt",
            "non-negative margins must leave a content area",
        );
    }
    errors
}

fn check_items(items: &[ListItem], depth: usize, path: &str, errors: &mut Vec<CheckError>) {
    if depth > 4 && !items.is_empty() {
        issue(errors, path, "list depth is at most 4");
        return;
    }
    for (i, item) in items.iter().enumerate() {
        check_items(
            &item.children,
            depth + 1,
            &format!("{path}[{i}].children"),
            errors,
        );
    }
}
fn check_block(block: &Block, theme: &Theme, path: &str, errors: &mut Vec<CheckError>) {
    if serde_json::to_vec(block).expect("block JSON").len() > 32 * 1024 {
        issue(errors, path, "block exceeds 32 KB");
    }
    if block
        .style
        .as_ref()
        .is_some_and(|s| !theme.text_styles.contains_key(s))
    {
        issue(
            errors,
            format!("{path}.style"),
            "unknown document text style",
        );
    }
    match &block.kind {
        Kind::Heading { level, .. } if !(1..=3).contains(level) => issue(
            errors,
            format!("{path}.level"),
            "heading level must be 1 to 3",
        ),
        Kind::List { items, .. } => check_items(items, 1, &format!("{path}.items"), errors),
        Kind::Table { rows, align, .. } => {
            if rows.len() > 200 {
                issue(errors, format!("{path}.rows"), "at most 200 rows");
            }
            let columns = rows.first().map_or(0, Vec::len);
            if columns > 20 || rows.iter().any(|row| row.len() > 20) {
                issue(errors, format!("{path}.rows"), "at most 20 columns");
            }
            if rows.iter().any(|r| r.len() != columns) {
                issue(
                    errors,
                    format!("{path}.rows"),
                    "table rows must have equal lengths",
                );
            }
            if !align.is_empty() && align.len() != columns {
                issue(errors, format!("{path}.align"), "one alignment per column");
            }
        }
        Kind::Image { src, .. } => {
            if src.starts_with("https://") {
                if let Err(e) = crate::remote_image::validate_url(src) {
                    issue(errors, format!("{path}.src"), e.to_string());
                }
            } else if !crate::deck::safe_asset_path(src) {
                issue(
                    errors,
                    format!("{path}.src"),
                    "expected HTTPS or a relative assets path without '..'",
                );
            }
        }
        Kind::Chart { chart }
            if chart.series.iter().any(|s| {
                s.values.len() != chart.labels.len() || s.values.iter().any(|v| !v.is_finite())
            }) =>
        {
            issue(
                errors,
                format!("{path}.chart.series"),
                "series must have one finite value per label",
            );
        }
        _ => {}
    }
}

impl Doc {
    /// Parse and check; only absent/duplicate ids and missing theme tokens are
    /// repaired. Every other error carries its JSON path.
    pub fn check(source: &str) -> Result<Self, Vec<CheckError>> {
        let mut value: Value = serde_json::from_str(source).map_err(|e| {
            vec![CheckError {
                path: "$".into(),
                message: e.to_string(),
            }]
        })?;
        let mut errors = Vec::new();
        if !value.is_object() {
            return Err(vec![CheckError {
                path: "$".into(),
                message: "expected document object".into(),
            }]);
        }
        if value["version"] != 1 {
            issue(&mut errors, "$.version", "expected document version 1");
        }
        let defaults = serde_json::to_value(Theme::default()).expect("theme JSON");
        if value.get("theme").is_none() {
            value["theme"] = defaults.clone();
        }
        if let Some(theme) = value["theme"].as_object_mut() {
            for (key, default) in defaults.as_object().expect("theme") {
                if let Some(map) = theme.get_mut(key).and_then(Value::as_object_mut) {
                    for (name, token) in default.as_object().expect("token map") {
                        map.entry(name).or_insert_with(|| token.clone());
                    }
                } else if !theme.contains_key(key) {
                    theme.insert(key.clone(), default.clone());
                }
            }
        }
        let theme = match serde_json::from_value::<Theme>(value["theme"].clone()) {
            Ok(theme) => theme,
            Err(e) => {
                issue(&mut errors, "$.theme", e.to_string());
                Theme::default()
            }
        };
        errors.extend(check_theme(&theme));
        let reserved: BTreeSet<String> = value["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["id"].as_str().map(str::to_string))
            .collect();
        let mut used = BTreeSet::new();
        if let Some(blocks) = value["blocks"].as_array_mut() {
            if blocks.len() > 2000 {
                issue(&mut errors, "$.blocks", "at most 2000 blocks");
            }
            for (i, block) in blocks.iter_mut().enumerate() {
                let path = format!("$.blocks[{i}]");
                if block.is_object() {
                    let id = block["id"].as_str().unwrap_or_default();
                    if (id.is_empty() || used.contains(id))
                        && block.get("id").is_none_or(Value::is_string)
                    {
                        let stem = format!("b{}", i + 1);
                        let mut id = stem.clone();
                        let mut n = 1;
                        while reserved.contains(&id) || used.contains(&id) {
                            id = format!("{stem}_{n}");
                            n += 1;
                        }
                        block["id"] = json!(id);
                    }
                    if let Some(id) = block["id"].as_str() {
                        used.insert(id.to_string());
                    }
                }
                match serde_json::from_value::<Block>(block.clone()) {
                    Ok(block) => check_block(&block, &theme, &path, &mut errors),
                    Err(e) => issue(&mut errors, path, e.to_string()),
                }
            }
        } else {
            issue(&mut errors, "$.blocks", "expected array");
        }
        let doc = match serde_json::from_value::<Self>(value) {
            Ok(doc) => {
                errors.extend(check_page(&doc.page));
                Some(doc)
            }
            Err(e) => {
                issue(&mut errors, "$", e.to_string());
                None
            }
        };
        if errors.is_empty() {
            Ok(doc.expect("checked doc"))
        } else {
            Err(errors)
        }
    }
}
