//! Model-facing prompt text bundled from `assets/prompts/`.
//! Edits change model behavior.

/// Workspace exploration and editing instructions for agent sessions.
pub const AGENT: &str = include_str!("../assets/prompts/agent.md");
/// Research planning instructions.
pub const RESEARCH_PLANNER: &str = include_str!("../assets/prompts/research/planner.md");
/// Research scoping instructions.
pub const RESEARCH_SURVEY: &str = include_str!("../assets/prompts/research/survey.md");
/// Research plan approval instructions.
pub const RESEARCH_PLAN_APPROVAL: &str =
    include_str!("../assets/prompts/research/plan-approval.md");
/// Research searcher instructions.
pub const RESEARCH_SEARCHER: &str = include_str!("../assets/prompts/research/searcher.md");
/// Shared research protocol and provider-cache anchor.
pub const RESEARCH_CACHE_PREFIX: &str = include_str!("../assets/prompts/research/cache-prefix.md");
/// Research synthesis instructions.
pub const RESEARCH_SYNTHESIZER: &str = include_str!("../assets/prompts/research/synthesizer.md");
/// Research critique instructions.
pub const RESEARCH_CRITIC: &str = include_str!("../assets/prompts/research/critic.md");
/// Research contradiction resolution instructions.
pub const RESEARCH_RESOLVER: &str = include_str!("../assets/prompts/research/resolver.md");
/// Research verification instructions.
pub const RESEARCH_VERIFIER: &str = include_str!("../assets/prompts/research/verifier.md");
/// Research final report instructions.
pub const RESEARCH_WRITER: &str = include_str!("../assets/prompts/research/writer.md");
/// Research topic derivation from a chat transcript.
pub const RESEARCH_TOPIC_FROM_CHAT: &str =
    include_str!("../assets/prompts/research/topic-from-chat.md");
/// Scanned-page transcription instructions.
pub const OCR: &str = include_str!("../assets/prompts/ocr.md");
/// Conversation compaction instructions.
pub const COMPACTION: &str = include_str!("../assets/prompts/compaction.md");
/// Durable memory extraction instructions.
pub const MEMORY: &str = include_str!("../assets/prompts/memory.md");
/// Session title and slug instructions.
pub const TITLE: &str = include_str!("../assets/prompts/title.md");
/// Swarm persona system instructions.
pub const SWARM_PERSONA: &str = include_str!("../assets/prompts/swarm/persona.md");
/// Swarm reply-turn instructions.
pub const SWARM_REPLY_TURN: &str = include_str!("../assets/prompts/swarm/reply-turn.md");
/// Swarm opening-turn instructions.
pub const SWARM_OPENING_TURN: &str = include_str!("../assets/prompts/swarm/opening-turn.md");
/// Swarm persona roster suggestion instructions.
pub const SWARM_ROSTER: &str = include_str!("../assets/prompts/swarm/roster.md");
/// Swarm convergence moderator instructions.
pub const SWARM_MODERATOR: &str = include_str!("../assets/prompts/swarm/moderator.md");
/// Swarm final synthesis instructions.
pub const SWARM_SYNTHESIS: &str = include_str!("../assets/prompts/swarm/synthesis.md");

/// Strip one trailing newline and substitute named placeholders in one pass.
/// Unknown keys panic in debug builds and remain unchanged in release builds.
#[must_use]
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut rest = template.strip_suffix('\n').unwrap_or(template);
    let mut rendered = String::with_capacity(rest.len());
    while let Some((before, after)) = rest.split_once("{{") {
        let Some((key, tail)) = after.split_once("}}") else {
            break;
        };
        rendered.push_str(before);
        let value = vars.iter().find(|(name, _)| *name == key);
        debug_assert!(value.is_some(), "unknown prompt placeholder: {key}");
        if let Some((_, value)) = value {
            rendered.push_str(value);
        } else {
            rendered.push_str("{{");
            rendered.push_str(key);
            rendered.push_str("}}");
        }
        rest = tail;
    }
    rendered.push_str(rest);
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const TEMPLATES: &[(&str, &[&str])] = &[
        (RESEARCH_PLANNER, &[]),
        (RESEARCH_SURVEY, &[]),
        (RESEARCH_PLAN_APPROVAL, &[]),
        (RESEARCH_SEARCHER, &[]),
        (RESEARCH_CACHE_PREFIX, &[]),
        (RESEARCH_SYNTHESIZER, &[]),
        (RESEARCH_CRITIC, &[]),
        (RESEARCH_RESOLVER, &[]),
        (RESEARCH_VERIFIER, &[]),
        (RESEARCH_WRITER, &[]),
        (RESEARCH_TOPIC_FROM_CHAT, &["conversation"]),
        (OCR, &[]),
        (COMPACTION, &[]),
        (MEMORY, &["facts", "user", "assistant"]),
        (TITLE, &["conversation"]),
        (SWARM_PERSONA, &["name", "blurb"]),
        (SWARM_REPLY_TURN, &["transcript", "round", "name"]),
        (SWARM_OPENING_TURN, &["round", "name"]),
        (SWARM_ROSTER, &["topic"]),
        (SWARM_MODERATOR, &["topic", "transcript"]),
        (SWARM_SYNTHESIS, &["topic", "transcript"]),
    ];

    #[test]
    fn agent_addendum_renders_without_placeholders() {
        let rendered = render(AGENT, &[]);
        assert!(rendered.starts_with("## Agent mode\n"));
        assert!(rendered.contains("The user approves every change and command."));
        assert!(AGENT.ends_with('\n'));
        assert!(!AGENT.ends_with("\n\n"));
        assert!(!rendered.contains("{{"));
    }

    #[test]
    fn render_fills_placeholders_in_one_pass() {
        assert_eq!(render("a {{x}} b\n", &[("x", "{{x}}")]), "a {{x}} b");
    }

    #[test]
    fn every_template_declares_exactly_its_placeholders() {
        for &(template, keys) in TEMPLATES {
            let mut found = BTreeSet::new();
            let mut rest = template;
            while let Some((_, after)) = rest.split_once("{{") {
                let (key, tail) = after.split_once("}}").expect("closed placeholder");
                found.insert(key);
                rest = tail;
            }
            assert_eq!(found, keys.iter().copied().collect());
            let vars: Vec<_> = keys.iter().map(|&key| (key, "value")).collect();
            assert!(!render(template, &vars).contains("{{"));
        }
    }

    #[test]
    fn prompt_files_end_with_one_newline() {
        for &(template, _) in TEMPLATES {
            assert!(template.ends_with('\n'));
            assert!(!template.ends_with("\n\n"));
        }
    }

    #[test]
    fn representative_prompts_match_their_previous_text() {
        assert_eq!(
            render(RESEARCH_CRITIC, &[]),
            "You are the critic stage of a research pipeline. Given the original topic and a draft report, decide if it's ready. Respond in exactly one of these forms:\n- the single word SATISFIED, if the draft thoroughly covers the topic with no notable gaps or contradictions.\n- GAPS: followed by a newline-separated bullet list (each line starting with '- ') of specific missing sub-topics or unanswered angles, each phrased as a searchable question.\n- CONTRADICTION: followed by one line describing a specific factual contradiction between sources in the draft that isn't resolved.\nUse CONTRADICTION only for an actual conflict between sources, not a missing angle — missing angles are always GAPS. Respond with nothing else."
        );
        let facts = "1. Prefers Rust";
        let user = "Remember my preference.";
        let assistant = "I will remember it.";
        assert_eq!(
            render(
                MEMORY,
                &[("facts", facts), ("user", user), ("assistant", assistant)],
            ),
            format!(
                "Stored facts (numbered, may be empty):\n{facts}\n\n\
                 Latest exchange:\nuser: {}\nassistant: {}\n\n\
                 Reply with ONLY a JSON array of memory ops, no markdown, no prose. \
                 Each op is one of:\n\
                 {{\"op\":\"add\",\"text\":\"<durable single-line fact>\"}}\n\
                 {{\"op\":\"update\",\"id\":<N>,\"text\":\"<replacement>\"}}\n\
                 {{\"op\":\"delete\",\"id\":<N>}}\n\
                 Empty array [] if nothing memory-worthy. Facts must be durable and \
                 user/project-relevant (preferences, identity, ongoing goals) — never a \
                 summary of what was just said. Merge/update instead of duplicating. \
                 Keep the total under 500 facts.",
                user, assistant,
            )
        );
    }
}
