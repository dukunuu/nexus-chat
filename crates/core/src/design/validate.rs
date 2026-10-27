//! Validation reuses deck validation for shared tokens and layout trees.
use super::{Design, derive_styles};
use crate::deck::{CheckError, Deck};
use serde_json::{Value, json};

fn check_slots(nodes: &[crate::deck::Node], path: &str, errors: &mut Vec<CheckError>) {
    for (index, node) in nodes.iter().enumerate() {
        let path = format!("{path}[{index}]");
        if !node.slot.is_empty()
            && !matches!(
                node.kind,
                crate::deck::Kind::Text { .. } | crate::deck::Kind::Image { .. }
            )
        {
            errors.push(CheckError {
                path: format!("{path}.slot"),
                message: "slots require text or image nodes".into(),
            });
        }
        check_slots(node.children(), &format!("{path}.children"), errors);
    }
}

impl Design {
    /// Parse, fill missing tokens/styles, repair layout ids and locate errors.
    #[allow(clippy::too_many_lines)] // Located checks for both media and every layout.
    pub fn check(source: &str) -> Result<Self, Vec<CheckError>> {
        let parse_error = |e: serde_json::Error| {
            vec![CheckError {
                path: "$".into(),
                message: e.to_string(),
            }]
        };
        let mut value: Value = serde_json::from_str(source).map_err(parse_error)?;
        // Validate raw trees separately, like Deck::check, so malformed nodes
        // report their layout/node path rather than failing at the root.
        let layouts = value
            .get_mut("slide")
            .and_then(Value::as_object_mut)
            .and_then(|slide| slide.remove("layouts"));
        let mut design: Self = serde_json::from_value(value).map_err(parse_error)?;
        let mut errors = Vec::new();
        let mut issue = |path: &str, message: &str| {
            errors.push(CheckError {
                path: path.into(),
                message: message.into(),
            });
        };
        if design.version != 1 {
            issue("$.version", "expected design version 1");
        }
        if design.name.trim().is_empty() {
            issue("$.name", "expected a design name");
        }
        if !design.scale.base_pt.is_finite() || !(1.0..=200.0).contains(&design.scale.base_pt) {
            issue("$.scale.base_pt", "expected 1 to 200 pt");
        }
        if !design.scale.ratio.is_finite() || !(1.0..=2.0).contains(&design.scale.ratio) {
            issue("$.scale.ratio", "expected 1 to 2");
        }
        if layouts
            .as_ref()
            .and_then(Value::as_object)
            .is_some_and(|map| map.len() > 40)
        {
            issue("$.slide.layouts", "at most 40 layouts");
        }
        if layouts.as_ref().is_some_and(|value| !value.is_object()) {
            issue("$.slide.layouts", "expected layout object");
        }
        for (key, value) in [
            ("margin", design.slide.spacing.margin),
            ("gap", design.slide.spacing.gap),
        ] {
            if !value.is_finite() || !(0.0..=2000.0).contains(&value) {
                issue(&format!("$.slide.spacing.{key}"), "expected 0 to 2000 px");
            }
        }
        if !(20..=200).contains(&design.doc.measure_ch) {
            issue("$.doc.measure_ch", "expected 20 to 200 characters");
        }
        let defaults = Self::default();
        for (key, value) in defaults.colors {
            design.colors.entry(key).or_insert(value);
        }
        for (key, value) in defaults.fonts {
            design.fonts.entry(key).or_insert(value);
        }
        let (slide, doc) = derive_styles(design.scale, &design.fonts);
        if design.slide.text_styles.is_empty() {
            design.slide.text_styles = slide;
        } else {
            for (key, value) in defaults.slide.text_styles {
                design.slide.text_styles.entry(key).or_insert(value);
            }
        }
        if design.doc.text_styles.is_empty() {
            design.doc.text_styles = doc;
        } else {
            for (key, value) in defaults.doc.text_styles {
                design.doc.text_styles.entry(key).or_insert(value);
            }
        }
        let theme = design.slide_theme();
        match Deck::check(
            &json!({"version": 2, "title": "Design", "theme": theme, "slides": []}).to_string(),
        ) {
            Ok(_) => {}
            Err(found) => errors.extend(found.into_iter().map(|mut e| {
                e.path = e
                    .path
                    .replace("$.theme.text_styles", "$.slide.text_styles")
                    .replace("$.theme.size", "$.slide.size")
                    .replace("$.theme.", "$.");
                e
            })),
        }
        for (name, layout) in layouts
            .as_ref()
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            match Deck::check(
                &json!({"version": 2, "title": name, "theme": theme, "slides": [layout]})
                    .to_string(),
            ) {
                Ok(mut deck) => {
                    let layout = deck.slides.remove(0);
                    check_slots(
                        &layout.children,
                        &format!("$.slide.layouts.{name}.children"),
                        &mut errors,
                    );
                    design.slide.layouts.insert(name.clone(), layout);
                }
                Err(found) => errors.extend(found.into_iter().map(|mut e| {
                    e.path = e
                        .path
                        .replace("$.slides[0]", &format!("$.slide.layouts.{name}"));
                    e
                })),
            }
        }
        let doc_theme = crate::doc::model::Theme::from_design(&design);
        errors.extend(
            crate::doc::validate::check_theme(&doc_theme)
                .into_iter()
                .map(|mut e| {
                    e.path = e
                        .path
                        .replace("$.theme.text_styles", "$.doc.text_styles")
                        .replace("$.theme.measure_ch", "$.doc.measure_ch")
                        .replace("$.theme.", "$.");
                    e
                }),
        );
        errors.extend(
            crate::doc::validate::check_page(&design.doc.page)
                .into_iter()
                .map(|mut e| {
                    e.path = e.path.replace("$.page", "$.doc.page");
                    e
                }),
        );
        if errors.is_empty() {
            Ok(design)
        } else {
            Err(errors)
        }
    }
}
