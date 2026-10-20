use std::fs;
use std::path::{Path, PathBuf};

use super::{App, AppCommand, WorkMode};
use crate::db::Db;
use crate::space::Space;

fn git(root: &Path, args: &[&str]) -> String {
    let output = crate::jobs::service::git(root, args).unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn repository_app() -> (PathBuf, App) {
    let dir = std::env::temp_dir().join(format!("nexus-improve-test-{}", uuid::Uuid::new_v4()));
    let repo = dir.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Improve Test"]);
    git(&repo, &["config", "user.email", "improve@example.invalid"]);
    fs::write(repo.join("tracked"), "HEAD version\n").unwrap();
    git(&repo, &["add", "tracked"]);
    git(&repo, &["commit", "-m", "initial"]);
    let mut app = App::new(
        Db::open_in_memory().unwrap(),
        Some("sk-or-test-key"),
        Space {
            root: dir.join("data"),
        },
    );
    // Persist the first user message, but do not start a provider or use the network.
    app.current_model = Some("local:offline".into());
    let config: toml::Value = toml::from_str(&format!(
        "[agent]\nnexus_repo = {:?}\n",
        repo.to_str().unwrap()
    ))
    .unwrap();
    app.saved.agent = config["agent"].clone().try_into().unwrap();
    (dir, app)
}

fn seed_failures(app: &App) {
    for (tool, action, kind, count, days, ok) in [
        ("fs", "edit", "stale_edit", 6, 0, 0),
        ("shell", "run", "exit", 5, 0, 0),
        ("scripts", "edit", "invalid_args", 4, 0, 0),
        ("app", "patch", "stale_edit", 3, 0, 0),
        ("files", "read", "not_found", 2, 0, 0),
        ("skills", "install", "invalid_args", 1, 0, 0),
        ("expired", "edit", "stale_edit", 20, 8, 0),
        ("successful", "edit", "", 20, 0, 1),
    ] {
        let at = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        for _ in 0..count {
            app.db
                .conn_for_test()
                .execute(
                    "INSERT INTO cache.tool_events
                     (at, session_id, tool, action, ok, error_kind, latency_ms, result_chars)
                     VALUES (?1, 'recent-session', ?2, ?3, ?4, ?5, 1, 1)",
                    (&at, tool, action, ok, kind),
                )
                .unwrap();
        }
    }
}

fn assert_improvement(app: &mut App, head: &str) -> PathBuf {
    assert_eq!(app.work_mode, WorkMode::Agent);
    let root = app.workspace.as_ref().unwrap().root().to_path_buf();
    assert!(!root.starts_with(&app.space.root));
    let branch = git(&root, &["branch", "--show-current"]);
    assert!(branch.starts_with("nexus/improve/fix-stale-edits-"));
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(root.join("tracked")).unwrap(),
        "HEAD version\n"
    );
    assert!(!root.join("untracked").exists());
    assert!(app.toolbox.defs().iter().any(|tool| tool.name == "fs"));
    let session = app.session.as_ref().unwrap();
    assert_eq!(app.db.session_workspace(&session.id), Some(root.clone()));
    assert_eq!(
        app.db.session_workspace_mode(&session.id).as_deref(),
        Some("agent")
    );
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].role, "user");
    assert_eq!(
        app.messages[0].content,
        concat!(
            "Improve nexus: Fix stale edits.\nRelevant recent tool failures:\n",
            "fs.edit: stale_edit ×6\nshell.run: exit ×5\n",
            "scripts.edit: invalid_args ×4\napp.patch: stale_edit ×3\n",
            "files.read: not_found ×2\n",
            "Acceptance: scripts/check.sh must pass. Work only in this worktree; finish with a summary of the change."
        )
    );
    let saved = app.db.load_messages(&session.id).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].content, app.messages[0].content);
    let status = app.last_status();
    assert!(status.contains(&branch));
    assert!(status.contains("review and merge yourself"));
    assert!(status.contains("git worktree remove"));
    root
}

#[test]
fn improve_requires_a_request_and_a_configured_checkout() {
    let mut app = super::tests::app_with_key();
    assert!(
        super::COMMANDS
            .iter()
            .any(|command| command.name == "improve")
    );
    assert_eq!(
        app.parse_command("improve   "),
        Err("usage: /improve <what>".into())
    );
    assert_eq!(
        app.parse_command("improve  Fix stale edits  "),
        Ok(AppCommand::Improve {
            what: "Fix stale edits".into()
        })
    );
    assert!(app.saved.agent.nexus_repo.is_none());
    for repo in [None, Some("   ".into())] {
        app.saved.agent.nexus_repo = repo;
        app.run_command("improve Fix stale edits").unwrap();
        let status = app.last_status();
        assert!(status.contains("[agent] nexus_repo = \"~/work/nexus-chat\""));
        assert!(status.contains("config.toml"));
        assert_eq!(app.work_mode, WorkMode::Chat);
        assert!(app.workspace.is_none());
        assert!(app.messages.is_empty());
        assert!(app.session.is_none());
    }
    let data = app.space.root.clone();
    drop(app);
    fs::remove_dir_all(data).unwrap();
}

#[test]
fn improve_isolates_head_and_seeds_a_fresh_or_empty_session_with_harness_rows() {
    for state in ["new", "empty", "messages"] {
        let (dir, mut app) = repository_app();
        let repo = dir.join("repo");
        let head = git(&repo, &["rev-parse", "HEAD"]);
        fs::write(repo.join("tracked"), "user's dirty work\n").unwrap();
        fs::write(repo.join("untracked"), "user's untracked work\n").unwrap();
        let status = git(&repo, &["status", "--porcelain"]);
        let previous = if state == "new" {
            None
        } else {
            let session = app
                .db
                .create_session("previous", "local:offline", &app.active_space.id, "chat")
                .unwrap();
            if state == "messages" {
                app.db
                    .add_user_message(&session.id, "previous request")
                    .unwrap();
            }
            app.switch_to_session_by_id(&session.id).unwrap();
            Some(session.id)
        };
        seed_failures(&app);
        app.run_command("improve Fix stale edits").unwrap();
        let root = assert_improvement(&mut app, &head);
        assert_eq!(git(&repo, &["branch", "--show-current"]), "main");
        assert_eq!(git(&repo, &["status", "--porcelain"]), status);
        assert_eq!(
            fs::read_to_string(repo.join("tracked")).unwrap(),
            "user's dirty work\n"
        );
        if let Some(previous) = previous {
            let current = &app.session.as_ref().unwrap().id;
            if state == "messages" {
                assert_ne!(current, &previous);
                let messages = app.db.load_messages(&previous).unwrap();
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].content, "previous request");
            } else {
                assert_eq!(current, &previous);
            }
        }
        drop(app);
        assert!(root.is_dir(), "review worktrees survive app exit");
        assert!(git(&root, &["branch", "--show-current"]).starts_with("nexus/improve/"));
        git(&repo, &["worktree", "remove", root.to_str().unwrap()]);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn repeated_improvements_get_distinct_review_branches_and_worktrees() {
    let (dir, mut app) = repository_app();
    let repo = dir.join("repo");
    let mut roots = Vec::new();
    let mut branches = Vec::new();
    for _ in 0..2 {
        app.run_command("improve Fix stale edits").unwrap();
        let root = app.workspace.as_ref().unwrap().root().to_path_buf();
        roots.push(root.clone());
        branches.push(git(&root, &["branch", "--show-current"]));
        assert!(
            app.messages[0]
                .content
                .contains("Relevant recent tool failures:\n\nAcceptance:")
        );
    }
    assert_ne!(roots[0], roots[1]);
    assert_ne!(branches[0], branches[1]);
    drop(app);
    for root in roots {
        assert!(root.is_dir());
        git(&repo, &["worktree", "remove", root.to_str().unwrap()]);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn improve_refuses_incognito_and_non_git_roots_before_creating_worktrees() {
    let (dir, mut app) = repository_app();
    let repo = dir.join("repo");
    app.incognito = true;
    app.run_command("improve Fix stale edits").unwrap();
    assert!(app.last_status().contains("incognito"));
    assert_eq!(git(&repo, &["branch", "--list"]), "* main");
    app.incognito = false;
    let non_git = dir.join("not-a-repository");
    fs::create_dir(&non_git).unwrap();
    app.saved.agent.nexus_repo = Some(non_git.display().to_string());
    assert!(app.run_command("improve Fix stale edits").is_err());
    assert_eq!(app.work_mode, WorkMode::Chat);
    assert!(app.workspace.is_none());
    assert!(app.messages.is_empty());
    drop(app);
    fs::remove_dir_all(dir).unwrap();
}
