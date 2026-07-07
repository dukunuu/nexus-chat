//! Skill files on disk: `<dir>/<name>/SKILL.md`, a small YAML-ish frontmatter
//! (`name`, `description`) followed by the instructions body. Parsed by hand
//! — the frontmatter is two lines, a real yaml crate would be overkill.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
}

pub fn skills_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("skills")
}

const WEB_SEARCH_SKILL: &str = include_str!("../assets/web-search-SKILL.md");
const FIND_SKILLS_SKILL: &str = include_str!("../assets/find-skills-SKILL.md");

/// Write the built-in skills on first run. Never overwrites — once installed
/// they're normal files the user can edit or delete like any other.
pub fn install_builtin(dir: &Path) {
    for (name, md) in [
        ("web-search", WEB_SEARCH_SKILL),
        ("find-skills", FIND_SKILLS_SKILL),
    ] {
        let path = dir.join(name).join("SKILL.md");
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, md);
    }
}

/// Load every `<dir>/*/SKILL.md` that parses. Missing dir → empty, not an error.
pub fn load_skills(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut skills: Vec<Skill> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let md = std::fs::read_to_string(e.path().join("SKILL.md")).ok()?;
            let (name, description) = parse_frontmatter(&md)?;
            Some(Skill {
                name,
                description,
                dir: e.path(),
            })
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Pull `name`/`description` out of `---\nname: x\ndescription: y\n---\n...`.
/// Values may optionally be quoted. Returns `None` if either field is missing.
pub fn parse_frontmatter(md: &str) -> Option<(String, String)> {
    let md = md.trim_start_matches('\u{feff}'); // strip a BOM if present
    let mut lines = md.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut name = None;
    let mut description = None;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some((key, val)) = trimmed.split_once(':') {
            let val = val.trim().trim_matches('"').trim_matches('\'').to_string();
            match key.trim() {
                "name" => name = Some(val),
                "description" => description = Some(val),
                _ => {}
            }
        }
    }
    Some((name?, description?))
}

/// Everything after the closing `---` of the frontmatter, trimmed. If there's
/// no frontmatter, the whole file is the body.
pub fn skill_body(md: &str) -> &str {
    let md = md.trim_start_matches('\u{feff}');
    if !md.trim_start().starts_with("---") {
        return md.trim();
    }
    let mut parts = md.splitn(3, "---");
    parts.next(); // before the first ---
    parts.next(); // frontmatter block
    parts.next().unwrap_or("").trim()
}

/// Parse `owner/repo/path/to/skill` (or bare `owner/repo` for a skill at the
/// repo root) into its three parts.
pub fn parse_gh_shorthand(s: &str) -> Option<(String, String, String)> {
    let mut parts = s.trim().trim_matches('/').splitn(3, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    let path = parts.next().unwrap_or("").to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo, path))
}

#[derive(serde::Deserialize)]
struct GhEntry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    download_url: Option<String>,
}

/// Fetch `SKILL.md` (and sibling files, one directory deep) from
/// `github.com/<owner>/<repo>/<path>` into `dest_root/<skill-name>/`.
/// Fails (and cleans up any partial download) unless a valid SKILL.md is found.
pub async fn install_from_github(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    path: &str,
    dest_root: &Path,
) -> anyhow::Result<String> {
    let entries = fetch_gh_contents(client, owner, repo, path).await?;
    anyhow::ensure!(
        entries
            .iter()
            .any(|e| e.name == "SKILL.md" && e.kind == "file"),
        "no SKILL.md at {owner}/{repo}/{path}"
    );
    let name = path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(repo)
        .to_string();
    let dest = dest_root.join(&name);
    let result = download_gh_entries(client, &entries, &dest, owner, repo, path, 1).await;
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    // Validate before declaring success — a SKILL.md with bad/missing frontmatter
    // shouldn't silently install as an unusable skill.
    let md = std::fs::read_to_string(dest.join("SKILL.md"))?;
    if parse_frontmatter(&md).is_none() {
        let _ = std::fs::remove_dir_all(&dest);
        anyhow::bail!("SKILL.md at {owner}/{repo}/{path} is missing name/description frontmatter");
    }
    Ok(name)
}

async fn fetch_gh_contents(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    path: &str,
) -> anyhow::Result<Vec<GhEntry>> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/contents/{path}");
    let resp = client
        .get(&url)
        .header("User-Agent", "nexus-chat")
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<GhEntry>>()
        .await?;
    Ok(resp)
}

/// Download a GitHub contents listing into `dest`, recursing into
/// subdirectories up to `depth_left` levels (skill resource folders are
/// small and shallow — no need to go further).
fn download_gh_entries<'a>(
    client: &'a reqwest::Client,
    entries: &'a [GhEntry],
    dest: &'a Path,
    owner: &'a str,
    repo: &'a str,
    repo_path: &'a str,
    depth_left: u8,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        std::fs::create_dir_all(dest)?;
        for entry in entries {
            match entry.kind.as_str() {
                "file" => {
                    let Some(url) = &entry.download_url else {
                        continue;
                    };
                    let bytes = client
                        .get(url)
                        .header("User-Agent", "nexus-chat")
                        .send()
                        .await?
                        .bytes()
                        .await?;
                    std::fs::write(dest.join(&entry.name), bytes)?;
                }
                "dir" if depth_left > 0 => {
                    let sub_repo_path = if repo_path.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{repo_path}/{}", entry.name)
                    };
                    let subentries = fetch_gh_contents(client, owner, repo, &sub_repo_path).await?;
                    download_gh_entries(
                        client,
                        &subentries,
                        &dest.join(&entry.name),
                        owner,
                        repo,
                        &sub_repo_path,
                        depth_left - 1,
                    )
                    .await?;
                }
                _ => {}
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_builtin_writes_both_skills_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("nexus-skills-{}", uuid::Uuid::new_v4()));
        install_builtin(&dir);
        let names: Vec<String> = load_skills(&dir).iter().map(|s| s.name.clone()).collect();
        assert_eq!(names, vec!["find-skills", "web-search"]);
        // never overwrites: user edits survive a re-run
        std::fs::write(
            dir.join("find-skills/SKILL.md"),
            "---\nname: mine\ndescription: d\n---\nx",
        )
        .unwrap();
        install_builtin(&dir);
        assert!(
            std::fs::read_to_string(dir.join("find-skills/SKILL.md"))
                .unwrap()
                .contains("mine")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let md = "---\nname: web-search\ndescription: Search the web\n---\nBody text here.";
        let (name, desc) = parse_frontmatter(md).unwrap();
        assert_eq!(name, "web-search");
        assert_eq!(desc, "Search the web");
        assert_eq!(skill_body(md), "Body text here.");
    }

    #[test]
    fn quoted_values_are_unquoted() {
        let md = "---\nname: \"a\"\ndescription: 'b'\n---\nx";
        assert_eq!(
            parse_frontmatter(md),
            Some(("a".to_string(), "b".to_string()))
        );
    }

    #[test]
    fn missing_frontmatter_is_none_but_body_is_whole_file() {
        let md = "just a plain file, no frontmatter";
        assert_eq!(parse_frontmatter(md), None);
        assert_eq!(skill_body(md), md);
    }

    #[test]
    fn missing_description_is_none() {
        let md = "---\nname: x\n---\nbody";
        assert_eq!(parse_frontmatter(md), None);
    }

    #[test]
    fn crlf_line_endings_still_parse() {
        let md = "---\r\nname: x\r\ndescription: y\r\n---\r\nbody\r\n";
        assert_eq!(
            parse_frontmatter(md),
            Some(("x".to_string(), "y".to_string()))
        );
    }

    #[test]
    fn parses_owner_repo_path_shorthand() {
        assert_eq!(
            parse_gh_shorthand("anthropics/skills/pdf"),
            Some((
                "anthropics".to_string(),
                "skills".to_string(),
                "pdf".to_string()
            ))
        );
    }

    #[test]
    fn parses_bare_owner_repo_as_root_path() {
        assert_eq!(
            parse_gh_shorthand("anthropics/skills"),
            Some((
                "anthropics".to_string(),
                "skills".to_string(),
                String::new()
            ))
        );
    }

    #[test]
    fn rejects_shorthand_missing_a_segment() {
        assert_eq!(parse_gh_shorthand("anthropics"), None);
        assert_eq!(parse_gh_shorthand(""), None);
    }

    #[test]
    fn multi_segment_path_survives_splitn() {
        assert_eq!(
            parse_gh_shorthand("a/b/nested/path/skill"),
            Some((
                "a".to_string(),
                "b".to_string(),
                "nested/path/skill".to_string()
            ))
        );
    }

    #[test]
    fn gh_contents_json_parses_into_entries() {
        let json = r#"[
            {"name": "SKILL.md", "type": "file", "download_url": "https://raw/SKILL.md"},
            {"name": "helpers", "type": "dir", "download_url": null}
        ]"#;
        let entries: Vec<GhEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "SKILL.md");
        assert_eq!(entries[0].kind, "file");
        assert_eq!(entries[1].kind, "dir");
    }
}
