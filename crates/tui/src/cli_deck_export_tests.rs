use super::*;

#[test]
fn deck_export_parses_and_runs_against_temp_space_files() {
    let temp = std::env::temp_dir().join(format!("nexus-cli-export-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(temp.join("decks")).unwrap();
    let deck = nexus_core::deck::Deck::titled("CLI story");
    std::fs::write(
        temp.join("decks/story.deck.json"),
        serde_json::to_vec(&deck).unwrap(),
    )
    .unwrap();
    for (format, extension) in [
        ("pptx", "pptx"),
        ("pdf", "pdf"),
        ("png", "zip"),
        ("html", "html"),
        ("json", "deck.json"),
    ] {
        let cli = Cli::try_parse_from([
            "nexus",
            "deck",
            "export",
            "decks/story.deck.json",
            "--format",
            format,
        ])
        .unwrap();
        let Some(Command::Deck { cmd }) = cli.command else {
            panic!("expected deck export");
        };
        deck_export(cmd, &temp).unwrap();
        assert!(
            temp.join(format!("exports/cli-story.{extension}"))
                .is_file()
        );
    }
    let custom = temp.join("custom.pptx");
    let cli = Cli::try_parse_from([
        "nexus",
        "deck",
        "export",
        temp.join("decks/story.deck.json").to_str().unwrap(),
        "--format",
        "pptx",
        "--out",
        custom.to_str().unwrap(),
    ])
    .unwrap();
    let Some(Command::Deck { cmd }) = cli.command else {
        panic!("expected deck export");
    };
    deck_export(cmd, &temp).unwrap();
    assert!(custom.is_file());
    assert!(Cli::try_parse_from(["nexus", "deck", "export", "x", "--format", "svg"]).is_err());
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn deck_import_parses_and_writes_a_new_deck_without_overwriting() {
    let temp = std::env::temp_dir().join(format!("nexus-cli-import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(temp.join("decks")).unwrap();
    let deck = nexus_core::deck::Deck::titled("CLI import");
    std::fs::write(
        temp.join("decks/story.deck.json"),
        serde_json::to_vec(&deck).unwrap(),
    )
    .unwrap();
    // Round trip through the exporter: the file is synthetic.
    let exported = nexus_core::deck::export::pptx::export_pptx(&deck, None).unwrap();
    std::fs::write(temp.join("talk.pptx"), exported).unwrap();
    let run = |args: &[&str]| {
        let mut full = vec!["nexus", "deck", "import"];
        full.extend(args);
        let cli = Cli::try_parse_from(full).unwrap();
        let Some(Command::Deck { cmd }) = cli.command else {
            panic!("expected deck import");
        };
        deck_export(cmd, &temp)
    };
    run(&["talk.pptx"]).unwrap();
    assert!(temp.join("decks/talk.deck.json").is_file());
    run(&["talk.pptx"]).unwrap();
    assert!(temp.join("decks/talk-2.deck.json").is_file());
    run(&["talk.pptx", "--out", "decks/named.deck.json"]).unwrap();
    assert!(temp.join("decks/named.deck.json").is_file());
    let again = run(&["talk.pptx", "--out", "decks/named.deck.json"]).unwrap_err();
    assert!(again.to_string().contains("never overwrite"), "{again}");
    let wrong = run(&["talk.pptx", "--out", "decks/named.json"]).unwrap_err();
    assert!(wrong.to_string().contains(".deck.json"), "{wrong}");
    assert!(run(&["missing.pptx"]).is_err());
    std::fs::remove_dir_all(temp).unwrap();
}
