//! Slide transitions and build timing, following the structure `PowerPoint`
//! itself writes: main sequence, one `par` per click, one `par` per
//! after-previous time slot, one `par` per effect.

use super::super::super::{
    AnimationSlot, Build, BuildBy, Direction, Easing, Effect, TransitionType, Trigger,
};
use super::{Slide, integer};
use std::collections::BTreeMap;
use std::fmt::Write as _;

const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const P14: &str = "http://schemas.microsoft.com/office/powerpoint/2010/main";
const P159: &str = "http://schemas.microsoft.com/office/powerpoint/2015/09/main";

/// What a node became on the slide, so builds can target it correctly.
#[derive(Clone, Copy, Debug)]
pub(super) struct Target {
    pub id: u32,
    /// A `p:sp` (text box or shape): the only targets that take `bldP`.
    pub shape: bool,
    /// Paragraph count of a text shape (None for everything else).
    pub paragraphs: Option<usize>,
}

fn direction(value: Direction) -> &'static str {
    match value {
        Direction::Left => "l",
        Direction::Right => "r",
        Direction::Up => "u",
        Direction::Down => "d",
    }
}

fn warn_once(warnings: &mut Vec<String>, text: &str) {
    if !warnings.iter().any(|w| w == text) {
        warnings.push(text.into());
    }
}

const EASING_NOTE: &str =
    "Transition and build easing curves are not exported; PowerPoint uses its own timing curves.";

pub(super) fn transition(slide: &Slide, warnings: &mut Vec<String>) -> String {
    let Some(t) = &slide.transition else {
        return String::new();
    };
    let dir = direction(t.direction.unwrap_or_default());
    let effect = match t.kind {
        TransitionType::None => return String::new(),
        TransitionType::Fade => "<p:fade/>".into(),
        TransitionType::Push => format!("<p:push dir=\"{dir}\"/>"),
        TransitionType::Cover => format!("<p:cover dir=\"{dir}\"/>"),
        TransitionType::Slide => format!("<p:pull dir=\"{dir}\"/>"),
        TransitionType::Zoom => "<p:zoom dir=\"in\"/>".into(),
        TransitionType::Smart => "<p159:morph option=\"byObject\"/>".into(),
    };
    if t.easing != Easing::Linear {
        warn_once(warnings, EASING_NOTE);
    }
    let speed = if t.duration_ms < 350.0 {
        "fast"
    } else if t.duration_ms < 800.0 {
        "med"
    } else {
        "slow"
    };
    let duration = integer(t.duration_ms);
    if t.kind == TransitionType::Smart {
        warnings.push(format!(
            "{}: smart transition exported as Morph (PowerPoint 2019 / 365, objects matched by name); older viewers get a fade.",
            slide.id
        ));
        return format!(
            "<mc:AlternateContent xmlns:mc=\"{MC}\"><mc:Choice xmlns:p159=\"{P159}\" xmlns:p14=\"{P14}\" Requires=\"p159\"><p:transition spd=\"{speed}\" p14:dur=\"{duration}\">{effect}</p:transition></mc:Choice><mc:Fallback><p:transition spd=\"{speed}\"><p:fade/></p:transition></mc:Fallback></mc:AlternateContent>"
        );
    }
    format!(
        "<mc:AlternateContent xmlns:mc=\"{MC}\"><mc:Choice xmlns:p14=\"{P14}\" Requires=\"p14\"><p:transition spd=\"{speed}\" p14:dur=\"{duration}\">{effect}</p:transition></mc:Choice><mc:Fallback><p:transition spd=\"{speed}\">{effect}</p:transition></mc:Fallback></mc:AlternateContent>"
    )
}

struct Ids(u32);

impl Ids {
    fn next(&mut self) -> u32 {
        self.0 += 1;
        self.0
    }
}

fn target(shape: u32, paragraph: Option<usize>) -> String {
    paragraph.map_or_else(
        || format!("<p:tgtEl><p:spTgt spid=\"{shape}\"/></p:tgtEl>"),
        |p| {
            format!(
                "<p:tgtEl><p:spTgt spid=\"{shape}\"><p:txEl><p:pRg st=\"{p}\" end=\"{p}\"/></p:txEl></p:spTgt></p:tgtEl>"
            )
        },
    )
}

fn visibility(ids: &mut Ids, tgt: &str, visible: bool, delay: i64) -> String {
    format!(
        "<p:set><p:cBhvr><p:cTn id=\"{}\" dur=\"1\" fill=\"hold\"><p:stCondLst><p:cond delay=\"{delay}\"/></p:stCondLst></p:cTn>{tgt}<p:attrNameLst><p:attrName>style.visibility</p:attrName></p:attrNameLst></p:cBhvr><p:to><p:strVal val=\"{}\"/></p:to></p:set>",
        ids.next(),
        if visible { "visible" } else { "hidden" }
    )
}

fn fade(ids: &mut Ids, tgt: &str, enter: bool, duration: i64) -> String {
    format!(
        "<p:animEffect transition=\"{}\" filter=\"fade\"><p:cBhvr><p:cTn id=\"{}\" dur=\"{duration}\"/>{tgt}</p:cBhvr></p:animEffect>",
        if enter { "in" } else { "out" },
        ids.next()
    )
}

fn numeric(
    ids: &mut Ids,
    tgt: &str,
    attribute: &str,
    from: &str,
    to: &str,
    duration: i64,
) -> String {
    format!(
        "<p:anim calcmode=\"lin\" valueType=\"num\"><p:cBhvr additive=\"base\"><p:cTn id=\"{}\" dur=\"{duration}\" fill=\"hold\"/>{tgt}<p:attrNameLst><p:attrName>{attribute}</p:attrName></p:attrNameLst></p:cBhvr><p:tavLst><p:tav tm=\"0\"><p:val><p:strVal val=\"{from}\"/></p:val></p:tav><p:tav tm=\"100000\"><p:val><p:strVal val=\"{to}\"/></p:val></p:tav></p:tavLst></p:anim>",
        ids.next()
    )
}

/// `(x, y)` formulas for a point off the slide on `side` (fly), or a short
/// distance from the resting place (rise).
fn offset(side: Direction, far: bool) -> (&'static str, &'static str) {
    match (side, far) {
        (Direction::Left, true) => ("0-#ppt_w/2", "#ppt_y"),
        (Direction::Right, true) => ("1+#ppt_w/2", "#ppt_y"),
        (Direction::Up, true) => ("#ppt_x", "0-#ppt_h/2"),
        (Direction::Down, true) => ("#ppt_x", "1+#ppt_h/2"),
        (Direction::Left, false) => ("#ppt_x-.04", "#ppt_y"),
        (Direction::Right, false) => ("#ppt_x+.04", "#ppt_y"),
        (Direction::Up, false) => ("#ppt_x", "#ppt_y-.06"),
        (Direction::Down, false) => ("#ppt_x", "#ppt_y+.06"),
    }
}

fn subtype(side: Direction) -> u8 {
    match side {
        Direction::Up => 1,
        Direction::Right => 2,
        Direction::Down => 4,
        Direction::Left => 8,
    }
}

/// One effect's preset identity and behaviours.
struct Rendered {
    preset: u8,
    subtype: u8,
    body: String,
}

fn render(
    ids: &mut Ids,
    tgt: &str,
    effect: Effect,
    build: &Build,
    slot: AnimationSlot,
    duration: i64,
) -> Rendered {
    let enter = slot == AnimationSlot::BuildIn;
    // A direction names the side an effect comes from (fly, rise) or starts at (wipe).
    let side = build.direction.unwrap_or(if effect == Effect::Rise {
        Direction::Down
    } else {
        Direction::Left
    });
    let mut body = String::new();
    let (preset, kind) = match effect {
        Effect::Appear => (1, 0),
        Effect::Fade => {
            body.push_str(&fade(ids, tgt, enter, duration));
            (10, 0)
        }
        Effect::Wipe => {
            let name = match side {
                Direction::Left => "left",
                Direction::Right => "right",
                Direction::Up => "up",
                Direction::Down => "down",
            };
            let _ = write!(
                body,
                "<p:animEffect transition=\"{}\" filter=\"wipe({name})\"><p:cBhvr><p:cTn id=\"{}\" dur=\"{duration}\"/>{tgt}</p:cBhvr></p:animEffect>",
                if enter { "in" } else { "out" },
                ids.next()
            );
            (22, subtype(side))
        }
        Effect::Fly | Effect::Rise => {
            let (x, y) = offset(side, effect == Effect::Fly);
            let (from_x, to_x, from_y, to_y) = if enter {
                (x, "#ppt_x", y, "#ppt_y")
            } else {
                ("#ppt_x", x, "#ppt_y", y)
            };
            body.push_str(&numeric(ids, tgt, "ppt_x", from_x, to_x, duration));
            body.push_str(&numeric(ids, tgt, "ppt_y", from_y, to_y, duration));
            if effect == Effect::Rise {
                body.push_str(&fade(ids, tgt, enter, duration));
                (42, 0)
            } else {
                (2, subtype(side))
            }
        }
        Effect::Scale => {
            for (attribute, rest, small) in [
                ("ppt_w", "#ppt_w", "#ppt_w*0.65"),
                ("ppt_h", "#ppt_h", "#ppt_h*0.65"),
            ] {
                let (from, to) = if enter { (small, rest) } else { (rest, small) };
                body.push_str(&numeric(ids, tgt, attribute, from, to, duration));
            }
            body.push_str(&fade(ids, tgt, enter, duration));
            (53, if enter { 16 } else { 32 })
        }
        Effect::Pulse | Effect::Shake => {
            // Grow/Shrink with auto-reverse reads as a pulse.
            let _ = write!(
                body,
                "<p:animScale><p:cBhvr><p:cTn id=\"{}\" dur=\"{duration}\" autoRev=\"1\" fill=\"hold\"/>{tgt}</p:cBhvr><p:by x=\"108000\" y=\"108000\"/></p:animScale>",
                ids.next()
            );
            (6, 0)
        }
        Effect::Blur | Effect::Typewriter => unreachable!("degraded before rendering"),
    };
    Rendered {
        preset,
        subtype: kind,
        body,
    }
}

struct Slot {
    delay: i64,
    end: i64,
    effects: Vec<String>,
}

struct Group {
    click: bool,
    slots: Vec<Slot>,
}

/// Degrade effects `PowerPoint` has no preset for, saying so.
fn degrade(effect: Effect, slot: AnimationSlot, node: &str, warnings: &mut Vec<String>) -> Effect {
    let mut say = |text: &str| warnings.push(format!("{node}: {text}"));
    match (effect, slot) {
        (Effect::Blur, _) => {
            say("blur build becomes fade.");
            Effect::Fade
        }
        (Effect::Typewriter, _) => {
            say("typewriter build becomes appear.");
            Effect::Appear
        }
        (Effect::Shake, _) => {
            say("shake emphasis becomes a pulse.");
            Effect::Shake
        }
        (Effect::Pulse, AnimationSlot::BuildIn | AnimationSlot::BuildOut) => {
            say("pulse used as an entrance or exit becomes fade.");
            Effect::Fade
        }
        (_, AnimationSlot::Emphasis) if effect != Effect::Pulse => {
            say("emphasis effect becomes a pulse.");
            Effect::Pulse
        }
        _ => effect,
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn builds(
    slide: &Slide,
    targets: &BTreeMap<String, Target>,
    warnings: &mut Vec<String>,
) -> String {
    let mut ids = Ids(2);
    let mut groups: Vec<Group> = Vec::new();
    let mut lists: BTreeMap<u32, bool> = BTreeMap::new();
    for (node, slot, build) in slide.build_sequence() {
        let Some(&found) = targets.get(&node.id) else {
            continue;
        };
        let effect = degrade(build.effect, slot, &node.id, warnings);
        let enter = slot == AnimationSlot::BuildIn;
        if build.easing != Easing::Linear {
            warn_once(warnings, EASING_NOTE);
        }
        let item_build = matches!(build.by, BuildBy::Item | BuildBy::Line);
        let by_paragraph = item_build && found.paragraphs.is_some_and(|n| n > 1);
        if build.by == BuildBy::Word {
            warnings.push(format!(
                "{}: word build animates the whole text box.",
                node.id
            ));
        }
        if item_build {
            warnings.push(format!(
                "{}: {}",
                node.id,
                if found.paragraphs.is_some() {
                    "item/line build animates PowerPoint paragraphs, not visual lines."
                } else {
                    "item/line build applies to text only; the whole object animates."
                }
            ));
        }
        if found.shape {
            *lists.entry(found.id).or_insert(false) |= by_paragraph;
        }
        let class = match slot {
            AnimationSlot::BuildIn => "entr",
            AnimationSlot::Emphasis => "emph",
            AnimationSlot::BuildOut => "exit",
        };
        let duration = if effect == Effect::Appear {
            0
        } else {
            integer(build.duration_ms).max(1)
        };
        let count = if by_paragraph {
            found.paragraphs.unwrap_or(1)
        } else {
            1
        };
        let stagger = integer(build.stagger_ms.unwrap_or(60.0));
        for index in 0..count {
            let trigger = if index == 0 {
                build.trigger
            } else {
                Trigger::WithPrevious
            };
            let delay = integer(build.delay_ms) + stagger * i64::try_from(index).unwrap_or(0);
            let tgt = target(found.id, by_paragraph.then_some(index));
            if trigger == Trigger::Click || groups.is_empty() {
                groups.push(Group {
                    click: trigger == Trigger::Click,
                    slots: vec![Slot {
                        delay: 0,
                        end: 0,
                        effects: Vec::new(),
                    }],
                });
            } else if trigger == Trigger::AfterPrevious {
                let group = groups.last_mut().expect("group exists");
                let end = group.slots.last().map_or(0, |s| s.end);
                group.slots.push(Slot {
                    delay: end,
                    end,
                    effects: Vec::new(),
                });
            }
            let node_type = match (index, trigger) {
                (0, Trigger::Click) => "clickEffect",
                (0, Trigger::AfterPrevious) => "afterEffect",
                _ => "withEffect",
            };
            let effect_id = ids.next();
            let rendered = render(&mut ids, &tgt, effect, build, slot, duration);
            let mut inner = String::new();
            if enter {
                inner.push_str(&visibility(&mut ids, &tgt, true, 0));
            }
            inner.push_str(&rendered.body);
            if slot == AnimationSlot::BuildOut {
                inner.push_str(&visibility(&mut ids, &tgt, false, (duration - 1).max(0)));
            }
            let grp = if found.shape { " grpId=\"0\"" } else { "" };
            let xml = format!(
                "<p:par><p:cTn id=\"{effect_id}\" presetID=\"{}\" presetClass=\"{class}\" presetSubtype=\"{}\" fill=\"hold\"{grp} nodeType=\"{node_type}\"><p:stCondLst><p:cond delay=\"{delay}\"/></p:stCondLst><p:childTnLst>{inner}</p:childTnLst></p:cTn></p:par>",
                rendered.preset, rendered.subtype
            );
            let group = groups.last_mut().expect("group exists");
            let current = group.slots.last_mut().expect("slot exists");
            current.end = current.end.max(current.delay + delay + duration);
            current.effects.push(xml);
        }
    }
    if groups.is_empty() {
        return String::new();
    }
    let mut sequence = String::new();
    for group in groups {
        let mut slots = String::new();
        for slot in group.slots {
            let _ = write!(
                slots,
                "<p:par><p:cTn id=\"{}\" fill=\"hold\"><p:stCondLst><p:cond delay=\"{}\"/></p:stCondLst><p:childTnLst>{}</p:childTnLst></p:cTn></p:par>",
                ids.next(),
                slot.delay,
                slot.effects.concat()
            );
        }
        // A sequence that does not start on a click begins with the slide.
        let start = if group.click {
            "<p:cond delay=\"indefinite\"/>"
        } else {
            "<p:cond delay=\"indefinite\"/><p:cond evt=\"onBegin\" delay=\"0\"><p:tn val=\"2\"/></p:cond>"
        };
        let _ = write!(
            sequence,
            "<p:par><p:cTn id=\"{}\" fill=\"hold\"><p:stCondLst>{start}</p:stCondLst><p:childTnLst>{slots}</p:childTnLst></p:cTn></p:par>",
            ids.next()
        );
    }
    let list = lists
        .iter()
        .fold(String::new(), |mut out, (shape, paragraphs)| {
            let attribute = if *paragraphs {
                "build=\"p\""
            } else {
                "animBg=\"1\""
            };
            let _ = write!(out, "<p:bldP spid=\"{shape}\" grpId=\"0\" {attribute}/>");
            out
        });
    let list = if list.is_empty() {
        String::new()
    } else {
        format!("<p:bldLst>{list}</p:bldLst>")
    };
    format!(
        "<p:timing><p:tnLst><p:par><p:cTn id=\"1\" dur=\"indefinite\" restart=\"never\" nodeType=\"tmRoot\"><p:childTnLst><p:seq concurrent=\"1\" nextAc=\"seek\"><p:cTn id=\"2\" dur=\"indefinite\" nodeType=\"mainSeq\"><p:childTnLst>{sequence}</p:childTnLst></p:cTn><p:prevCondLst><p:cond evt=\"onPrev\" delay=\"0\"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:prevCondLst><p:nextCondLst><p:cond evt=\"onNext\" delay=\"0\"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:nextCondLst></p:seq></p:childTnLst></p:cTn></p:par></p:tnLst>{list}</p:timing>"
    )
}
