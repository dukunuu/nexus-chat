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
