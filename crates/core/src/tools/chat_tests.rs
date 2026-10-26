use super::*;
use crate::db::Db;
use crate::tools::{ToolBox, is_read_only_tool};
use serde_json::json;

fn fixture() -> (Db, FilesCtx, String) {
    let root = std::env::temp_dir().join(format!("nexus-chat-tool-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("nexus.db");
    let db = Db::open(&path).unwrap();
    let space = db.list_spaces().unwrap()[0].id.clone();
    let session = db
        .create_session("Earlier discussion", "test", &space, "chat")
        .unwrap()
        .id;
    let ctx = FilesCtx {
        db_path: path,
        space_id: space,
        embedder: None,
    };
    (db, ctx, session)
}

fn toolbox(ctx: &FilesCtx) -> ToolBox {
    ToolBox::new(
        std::path::PathBuf::new(),
        None,
        None,
        "auto".to_string(),
        Vec::new(),
        Some(ctx.db_path.clone()),
        Some(ctx.clone()),
        None,
    )
}

fn cleanup(db: Db, ctx: &FilesCtx) {
    drop(db);
    std::fs::remove_dir_all(ctx.db_path.parent().unwrap()).unwrap();
}

fn hit_url(result: &str) -> String {
    result
        .lines()
        .find_map(|line| line.trim().strip_prefix("chat:"))
        .map(|rest| format!("chat:{rest}"))
        .expect("a chat: URL in the hit")
}

#[tokio::test]
async fn chats_search_mode_is_read_only_and_hits_open_with_fetch_url() {
    let (db, ctx, session) = fixture();
    db.add_user_message(&session, "remember the telescope purchase")
        .unwrap();
    let mut tb = toolbox(&ctx);
    let defs = tb.defs();
    assert!(!defs.iter().any(|d| d.name == "chats"), "no separate tool");
    let search = defs.iter().find(|d| d.name == "search").unwrap();
    assert!(search.description.contains("mode=chats"));
    let fetch = defs.iter().find(|d| d.name == "fetch_url").unwrap();
    assert!(fetch.description.contains("chat:"));
    let args = r#"{"mode":"chats","query":"telescope"}"#;
    assert!(is_read_only_tool("search", args));
    let result = tb.run("search", args).await.0;
    assert!(
        result.starts_with("past chats (keyword search)"),
        "{result}"
    );
    assert!(result.contains("[1] Earlier discussion"), "{result}");
    assert!(result.contains("· user"), "{result}");
    let url = hit_url(&result);
    assert!(url.starts_with(&format!("chat:{session}#")), "{url}");
    let read = tb
        .run("fetch_url", &json!({ "url": url }).to_string())
        .await
        .0;
    assert!(read.contains("remember the telescope purchase"), "{read}");
    let calls = json!({"calls":[
        {"tool":"search", "arguments":{"mode":"chats", "query":"telescope"}},
        {"tool":"fetch_url", "arguments":{"url":url}}
    ]});
    let batch = tb.run("batch", &calls.to_string()).await.0;
    assert!(batch.contains("[1/2] search chats"), "{batch}");
    assert!(batch.contains("[2/2] fetch_url chat:"), "{batch}");
    // The chat being had is already in context.
    tb.session_id = session.clone();
    assert!(tb.run("search", args).await.0.contains("no matches"));
    tb.research_only = true;
    assert!(
        tb.run("fetch_url", &json!({ "url": url }).to_string())
            .await
            .0
            .contains("chat history unavailable")
    );
    cleanup(db, &ctx);
}

#[test]
fn transcript_starts_near_the_hit_and_collapses_tool_noise() {
    let (db, ctx, session) = fixture();
    db.add_user_message(&session, "first").unwrap();
    db.add_user_message(&session, "second").unwrap();
    db.add_user_message(&session, "third").unwrap();
    let center = db
        .add_assistant_message(
            &session,
            "center answer",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    db.add_tool_call_message(
        &session,
        "files/read notes\nlarge noisy payload must disappear",
    )
    .unwrap();
    let (text, start) = transcript(&ctx, &format!("chat:{session}#{center}")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    // Two messages of context ahead of the hit.
    assert!(
        lines[start - 1].ends_with("] user"),
        "{:?}",
        &lines[start - 1]
    );
    assert_eq!(lines[start], "second");
    assert!(text.contains("center answer"));
    assert!(text.contains("files/read notes"));
    assert!(!text.contains("noisy payload"));
    assert_eq!(transcript(&ctx, &format!("chat:{session}")).unwrap().1, 1);
    let missing = transcript(&ctx, &format!("chat:{session}#nope")).unwrap_err();
    assert!(missing.to_string().contains("not in this chat"));
    cleanup(db, &ctx);
}

#[test]
fn another_spaces_chat_is_out_of_reach_even_by_id() {
    let (db, ctx, _) = fixture();
    let other = db.create_space("private").unwrap();
    let hidden = db
        .create_session("Hidden", "test", &other.id, "chat")
        .unwrap()
        .id;
    db.add_user_message(&hidden, "secret").unwrap();
    let error = transcript(&ctx, &format!("chat:{hidden}")).unwrap_err();
    assert!(error.to_string().contains("in this space"), "{error}");
    cleanup(db, &ctx);
}

#[tokio::test]
async fn long_chats_page_by_line_and_a_huge_line_is_split() {
    let (db, ctx, session) = fixture();
    let text = format!("START {} END", "漢字é ".repeat(2000));
    db.add_user_message(&session, &text).unwrap();
    for n in 0..300 {
        db.add_user_message(&session, &format!("line {n}")).unwrap();
    }
    let (full, _) = transcript(&ctx, &format!("chat:{session}")).unwrap();
    assert!(full.lines().all(|line| line.chars().count() <= LINE_CHARS));
    assert!(full.lines().filter(|l| l.contains('漢')).count() > 1);
    let tb = toolbox(&ctx);
    let first = tb
        .run(
            "fetch_url",
            &json!({"url": format!("chat:{session}")}).to_string(),
        )
        .await
        .0;
    assert!(first.contains("(lines 1-200 of"), "{}", &first[..200]);
    let next = tb
        .run(
            "fetch_url",
            // A header, the text and a blank line per message: 300 short
            // messages run past line 900.
            &json!({"url": format!("chat:{session}"), "offset": 801}).to_string(),
        )
        .await
        .0;
    assert!(next.contains("line 299"), "{}", &next[..200]);
    cleanup(db, &ctx);
}
