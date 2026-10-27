//! Design extraction uses exactly the presentation importer's inheritance and art reader.
// OPC part names are case sensitive.
#![allow(clippy::case_sensitive_file_extension_comparisons)]
use super::*;
use crate::design::{Design, derive_styles};

pub(crate) fn theme_tokens(root: &El) -> DeckTheme {
    let theme = Theme::from_xml(root, None);
    let master = Master {
        root: El::default(),
        rels: Rels::default(),
        theme,
        title: ListStyle::default(),
        body: ListStyle::default(),
        other: ListStyle::default(),
    };
    let mut env = environment(1920.0 / 12_192_000.0);
    deck_theme(&mut env, &master);
    env.deck_theme
}

fn environment(scale: f64) -> Env {
    Env {
        scale,
        default_style: ListStyle::default(),
        roles: [("bg", Rgba::rgb(255, 255, 255)); 4],
        heading: "Inter".into(),
        body: "Inter".into(),
        deck_theme: DeckTheme::default(),
    }
}

fn lock(nodes: &mut [Node]) {
    for node in nodes {
        node.locked = true;
        node.template_background = true;
        if let Some(children) = node.children_mut() {
            lock(children);
        }
    }
}

#[allow(clippy::too_many_lines)] // One bounded package-to-design conversion.
pub(crate) fn extract_design(bytes: &[u8], assets: &Path) -> Result<(Design, Vec<String>)> {
    let mut pkg = Package::open(bytes)?;
    let presentation = pkg.xml("ppt/presentation.xml")?;
    let size = presentation
        .kid("sldSz")
        .context("template has no slide size")?;
    let width = size.num("cx").context("template has no slide width")?;
    let height = size.num("cy").context("template has no slide height")?;
    anyhow::ensure!(width > 0.0 && height > 0.0, "invalid template slide size");
    let mut parts = Parts::default();
    let layouts: Vec<_> = pkg
        .names()
        .into_iter()
        .filter(|n| {
            n.starts_with("ppt/slideLayouts/") && n.ends_with(".xml") && !n.contains("/_rels/")
        })
        .collect();
    let mut usable = Vec::new();
    let mut warnings = Warnings::default();
    for name in layouts {
        if parts.ensure_layout(&mut pkg, &name) {
            usable.push(name);
        } else {
            warnings.push(format!("layout {name}: missing layout or master; skipped"));
        }
    }
    let first_layout = usable
        .first()
        .context("template has no usable slide layouts")?;
    let first = &parts.masters[&parts.layouts[first_layout].master];
    let mut env = environment(1920.0 / width);
    if let Some(style) = presentation.kid("defaultTextStyle") {
        env.default_style = list_style(style, &first.theme);
    }
    deck_theme(&mut env, first);
    let mut design = Design {
        colors: env.deck_theme.colors.clone(),
        fonts: env.deck_theme.fonts.clone(),
        ..Design::default()
    };
    design.slide.size = SlideSize {
        w: 1920.0,
        h: round2(height * env.scale).clamp(180.0, 4320.0),
    };
    design.slide.text_styles = env.deck_theme.text_styles.clone();
    for (key, level) in [("subheading", 1), ("lead", 2)] {
        let mut style = first.body.clone();
        style.0[0] = first.body.level(level).clone();
        design
            .slide
            .text_styles
            .insert(key.into(), style_font(&env, first, &style, VAlign::Top));
    }
    design.slide.text_styles.insert(
        "caption".into(),
        style_font(&env, first, &first.other, VAlign::Top),
    );
    design.scale.base_pt =
        (design.slide.text_styles["body"].size.unwrap_or(42.0) * 11.0 / 42.0).clamp(1.0, 200.0);
    let title = design.slide.text_styles["title"].size.unwrap_or(120.0);
    let body = design.slide.text_styles["body"].size.unwrap_or(42.0);
    design.scale.ratio = (title / body).powf(0.25).clamp(1.0, 2.0);
    design.doc.text_styles = derive_styles(design.scale, &design.fonts).1;
    design.slide.layouts.clear();
    let mut st = State {
        pkg,
        assets: assets.into(),
        warn: warnings,
        next: 0,
        images: HashMap::new(),
        placeholder: None,
        spids: HashMap::new(),
        budget: MAX_NODES,
        limit_said: false,
    };
    for part in usable {
        if design.slide.layouts.len() == 40 {
            st.warn
                .push("More than 40 layouts; remaining layouts skipped".into());
            break;
        }
        st.images.clear();
        let layout = &parts.layouts[&part];
        let master = &parts.masters[&layout.master];
        let name = layout
            .root
            .kid("cSld")
            .and_then(|c| c.attr("name"))
            .unwrap_or("Layout");
        let slug = crate::doc::export::slug(name);
        let mut key = slug.clone();
        for index in 2.. {
            if !design.slide.layouts.contains_key(&key) {
                break;
            }
            key = format!("{slug}-{index}");
        }
        // Converting an empty slide brings in only layout/master decoration.
        let root = super::super::xml::parse("<p:sld><p:cSld><p:spTree/></p:cSld></p:sld>")?;
        let mut slide = convert_slide(
            &mut st,
            &env,
            master,
            Some(layout),
            &root,
            &Rels::default(),
            &part,
            1,
            design.slide.size,
        );
        lock(&mut slide.children);
        slide.name = Some(name.chars().take(80).collect());
        slide.source_layout = Some(part.clone());
        slide.id.clone_from(&key);
        let cx = Cx {
            env: &env,
            theme: &master.theme,
            master,
            layout_tree: None,
            master_tree: master.tree(),
            rels: &layout.rels,
            part: &part,
            slide: 1,
            record: false,
            decor: false,
        };
        let mut counts = BTreeMap::new();
        if let Some(tree) = layout.tree() {
            for shape in &tree.kids {
                let Some(ph) = shapes::ph(shape) else {
                    continue;
                };
                let ty = ph.attr("type").unwrap_or("obj");
                let slot = match ty {
                    "title" | "ctrTitle" => "title",
                    "body" | "obj" => "body",
                    "pic" => "image",
                    "subTitle" => "subtitle",
                    "dt" | "ftr" | "sldNum" | "hdr" => {
                        st.warn
                            .tally("Date, footer and slide-number placeholders skipped");
                        continue;
                    }
                    _ => {
                        st.warn.names("Unsupported placeholders skipped", ty);
                        continue;
                    }
                };
                let count = counts.entry(slot).or_insert(0);
                *count += 1;
                let slot = if *count == 1 {
                    slot.into()
                } else {
                    format!("{slot}{count}")
                };
                if let Some(node) = shapes::template_slot(&cx, &mut st, shape, &slot) {
                    slide.children.push(node);
                } else {
                    st.warn.push(format!(
                        "{name}: placeholder {slot} has no usable geometry; skipped"
                    ));
                }
            }
        }
        design.slide.layouts.insert(key, slide);
    }
    Ok((design, st.warn.finish()))
}
