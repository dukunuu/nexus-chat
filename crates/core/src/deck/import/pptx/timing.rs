//! Slide transitions and entrance, emphasis and exit animations.

use std::collections::HashMap;

use crate::deck::{
    Animation, AnimationSlot, Build, BuildBy, Direction, Effect, Kind, Node, Slide, Transition,
    TransitionType, Trigger,
};

use super::super::xml::El;
use super::Warnings;

fn direction(value: &str) -> Direction {
    match value {
        "r" => Direction::Right,
        "u" => Direction::Up,
        "d" => Direction::Down,
        _ => Direction::Left,
    }
}

/// The `p:transition` of a slide, preferring the `PowerPoint` 2010 form that
/// carries a duration.
fn transition_el(root: &El) -> Option<&El> {
    if let Some(found) = root.kid("transition") {
        return Some(found);
    }
    root.kids_named("AlternateContent").find_map(|ac| {
        ac.kid("Choice")
            .and_then(|c| c.kid("transition"))
            .or_else(|| ac.kid("Fallback").and_then(|f| f.kid("transition")))
    })
}

pub(super) fn transition(root: &El, warn: &mut Warnings) -> Option<Transition> {
    let el = transition_el(root)?;
    let effect = el.kids.first()?;
    let (kind, directional) = match effect.name.as_str() {
        "fade" => (TransitionType::Fade, false),
        "push" => (TransitionType::Push, true),
        "cover" => (TransitionType::Cover, true),
        "pull" => (TransitionType::Slide, true),
        "zoom" => (TransitionType::Zoom, false),
        "morph" => (TransitionType::Smart, false),
        "cut" | "AlternateContent" => return None,
        _ => {
            warn.tally("Transitions without a deck equivalent are imported as fade");
            (TransitionType::Fade, false)
        }
    };
    let duration = el
        .attr_local("dur")
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or_else(|| match el.attr("spd") {
            Some("fast") => 250.0,
            Some("slow") => 800.0,
            _ => 500.0,
        });
    let mut out = Transition {
        kind,
        duration_ms: duration.clamp(0.0, 5000.0),
        ..Transition::default()
    };
    if directional {
        out.direction = Some(direction(effect.attr("dir").unwrap_or("l")));
    }
    Some(out)
}

// -- animations ----------------------------------------------------------------

struct Parsed {
    spid: String,
    slot: AnimationSlot,
    build: Build,
    /// Targets one paragraph of its shape.
    paragraph: bool,
}

fn effect_of(
    class: &str,
    preset: u32,
    subtype: u32,
    warn: &mut Warnings,
) -> (Effect, Option<Direction>) {
    let side = match subtype {
        1 => Some(Direction::Up),
        2 => Some(Direction::Right),
        4 => Some(Direction::Down),
        8 => Some(Direction::Left),
        _ => None,
    };
    if class == "emph" {
        if !matches!(preset, 6 | 26) {
            warn.tally("Emphasis effects other than pulse are imported as a pulse");
        }
        return (Effect::Pulse, None);
    }
    match preset {
        1 => (Effect::Appear, None),
        10 => (Effect::Fade, None),
        2 => (Effect::Fly, side),
        22 => (Effect::Wipe, side),
        42 => (Effect::Rise, None),
        53 | 16 => (Effect::Scale, None),
        _ => {
            warn.tally("Animation effects without a deck equivalent are imported as fade");
            (Effect::Fade, None)
        }
    }
}

fn parse_effect(ctn: &El, warn: &mut Warnings) -> Option<Parsed> {
    let slot = match ctn.attr("presetClass")? {
        "entr" => AnimationSlot::BuildIn,
        "exit" => AnimationSlot::BuildOut,
        "emph" => AnimationSlot::Emphasis,
        _ => return None,
    };
    let class = ctn.attr("presetClass")?;
    let preset = ctn.num("presetID").map_or(0, |v| v as u32);
    let subtype = ctn.num("presetSubtype").map_or(0, |v| v as u32);
    let mut targets = Vec::new();
    ctn.descendants("spTgt", &mut targets);
    let target = targets.first()?;
    let spid = target.attr("spid")?.to_string();
    let paragraph = target.kid("txEl").is_some();
    let (effect, side) = effect_of(class, preset, subtype, warn);
    let trigger = match ctn.attr("nodeType") {
        Some("withEffect") => Trigger::WithPrevious,
        Some("afterEffect") => Trigger::AfterPrevious,
        _ => Trigger::Click,
    };
    let delay = ctn
        .kid("stCondLst")
        .and_then(|l| l.kid("cond"))
        .and_then(|c| c.num("delay"))
        .unwrap_or(0.0);
    let mut timings = Vec::new();
    ctn.descendants("cTn", &mut timings);
    let duration = timings
        .iter()
        .filter_map(|t| t.num("dur"))
        .filter(|d| *d > 1.0)
        .fold(0.0_f64, f64::max);
    Some(Parsed {
        spid,
        slot,
        build: Build {
            effect,
            direction: side,
            duration_ms: if effect == Effect::Appear {
                0.0
            } else {
                duration.clamp(0.0, 5000.0)
            },
            delay_ms: delay.clamp(0.0, 10_000.0),
            trigger,
            ..Build::default()
        },
        paragraph,
    })
}

fn node_mut<'a>(nodes: &'a mut [Node], id: &str) -> Option<&'a mut Node> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        if let Kind::Frame { children, .. } = &mut node.kind
            && let Some(found) = node_mut(children, id)
        {
            return Some(found);
        }
    }
    None
}

/// Entrance, emphasis and exit effects of the main sequence onto `slide`.
pub(super) fn builds(
    root: &El,
    slide: &mut Slide,
    spids: &HashMap<String, String>,
    warn: &mut Warnings,
) {
    let Some(timing) = root.find("timing") else {
        return;
    };
    let mut containers = Vec::new();
    timing.descendants("cTn", &mut containers);
    let Some(main) = containers
        .into_iter()
        .find(|c| c.attr("nodeType") == Some("mainSeq"))
    else {
        return;
    };
    let mut parsed: Vec<Parsed> = Vec::new();
    let mut stagger: HashMap<String, f64> = HashMap::new();
    let mut unsupported = 0;
    let clicks = main
        .kid("childTnLst")
        .map(|l| l.kids_named("par"))
        .into_iter()
        .flatten();
    for click in clicks {
        let slots = click
            .path(&["cTn", "childTnLst"])
            .map(|l| l.kids_named("par"))
            .into_iter()
            .flatten();
        for time_slot in slots {
            let effects = time_slot
                .path(&["cTn", "childTnLst"])
                .map(|l| l.kids_named("par"))
                .into_iter()
                .flatten();
            for effect in effects {
                let Some(ctn) = effect.kid("cTn") else {
                    continue;
                };
                match parse_effect(ctn, warn) {
                    Some(next) => {
                        // Paragraph builds of one shape are one item build.
                        if next.paragraph
                            && let Some(previous) = parsed.last()
                            && previous.paragraph
                            && previous.spid == next.spid
                            && previous.slot == next.slot
                        {
                            stagger
                                .entry(next.spid.clone())
                                .or_insert(next.build.delay_ms - previous.build.delay_ms);
                            continue;
                        }
                        parsed.push(next);
                    }
                    None => unsupported += 1,
                }
            }
        }
    }
    if unsupported > 0 {
        warn.tally("Motion paths and other animations are not imported");
    }
    let mut order: Vec<String> = Vec::new();
    for item in parsed {
        let Some(id) = spids.get(&item.spid) else {
            continue;
        };
        let Some(node) = node_mut(&mut slide.children, id) else {
            continue;
        };
        let is_text = matches!(node.kind, Kind::Text { .. });
        let mut build = item.build;
        if item.paragraph && is_text {
            build.by = BuildBy::Item;
            build.stagger_ms = stagger.get(&item.spid).map(|s| s.clamp(0.0, 10_000.0));
        }
        if !is_text && build.effect == Effect::Typewriter {
            build.effect = Effect::Fade;
        }
        let animation = node.animation.get_or_insert_with(Animation::default);
        let slot = match item.slot {
            AnimationSlot::BuildIn => &mut animation.build_in,
            AnimationSlot::Emphasis => &mut animation.emphasis,
            AnimationSlot::BuildOut => &mut animation.build_out,
        };
        if slot.is_some() {
            warn.tally("Shapes with two effects of one kind keep only the first");
            continue;
        }
        *slot = Some(build);
        if !order.contains(id) {
            order.push(id.clone());
        }
    }
    slide.builds = order;
}
