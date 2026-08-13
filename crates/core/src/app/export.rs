//! `/export`: write a research session's latest report + bibliography to a
//! markdown file in the active space's files dir, overwritten on every run
//! so it stays a living document.

use anyhow::Result;
use std::fmt::Write as _;

/// Append a numbered `## Sources` bibliography built from `citations`
/// (`(report_file, url, title)` rows, as returned by `Db::search_citations`)
/// to `report_body`. No section at all when `citations` is empty — an
/// export with nothing to cite shouldn't show an empty heading.
pub fn assemble_report(report_body: &str, citations: &[(String, String, String)]) -> String {
    if citations.is_empty() {
        return report_body.to_string();
    }
    let mut out = report_body.trim_end().to_string();
    out.push_str("\n\n## Sources\n\n");
    for (i, (_, url, title)) in citations.iter().enumerate() {
        if title.is_empty() {
            let _ = writeln!(out, "{}. {url}", i + 1);
        } else {
            let _ = writeln!(out, "{}. {title} — {url}", i + 1);
        }
    }
    out
}

impl super::App {
    /// `/export`: write the active session's latest research report (the
    /// most recent `assistant` message) plus its citations to
    /// `<space>/files/reports/<session-slug>.md`, overwriting any earlier
    /// export of the same session. No-op with a status message if the
    /// session has no research report yet.
    pub fn export_report(&mut self) -> Result<()> {
        let Some(session) = &self.session else {
            self.push_status("no active session".to_string());
            return Ok(());
        };
        let Some(report) = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .map(|m| m.content.clone())
        else {
            self.push_status("nothing to export — no assistant reply yet".to_string());
            return Ok(());
        };
        let citations = self.db.search_citations(&self.active_space.id, None)?;
        let cited_here: Vec<(String, String, String)> = {
            let urls_in_report: std::collections::HashSet<String> =
                crate::citations::parse_citations(&report)
                    .into_iter()
                    .map(|(_, url)| url)
                    .collect();
            citations
                .into_iter()
                .filter(|(_, url, _)| urls_in_report.contains(url))
                .collect()
        };
        let assembled = assemble_report(&report, &cited_here);
        let dir = self
            .space
            .files_dir(&self.active_space.name)
            .join("reports");
        std::fs::create_dir_all(&dir)?;
        let slug = super::sessions::slugify(&session.title);
        let path = dir.join(format!("{slug}.md"));
        std::fs::write(&path, assembled)?;
        self.push_status(format!("exported to {}", path.display()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_report_appends_numbered_bibliography() {
        let citations = vec![
            (
                "report-a.md".to_string(),
                "https://a.example".to_string(),
                "Title A".to_string(),
            ),
            (
                "report-a.md".to_string(),
                "https://b.example".to_string(),
                "".to_string(),
            ),
        ];
        let out = assemble_report("# Report\nBody [1] [2].", &citations);
        assert!(out.contains("## Sources"));
        assert!(out.contains("1. Title A — https://a.example"));
        assert!(out.contains("https://b.example"));
        assert!(out.starts_with("# Report"));
    }

    #[test]
    fn assemble_report_with_no_citations_has_no_sources_section() {
        let out = assemble_report("# Report\nBody.", &[]);
        assert!(!out.contains("## Sources"));
    }
}
