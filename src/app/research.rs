//! Deep research: a background multi-agent pipeline triggered by `/research`.

/// A background research pipeline update: a phase label, or the final
/// report/error.
pub(crate) enum ResearchUpdate {
    Stage(String),
    Done(std::result::Result<String, String>),
}
