use super::*;
#[test]
fn doc_cli_import_export_and_no_overwrite() {
    let temp = std::env::temp_dir().join(format!("nexus-cli-doc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("report.md"),
        "# CLI report\n\nA document.\n\n## Findings\n\n**Evidence**.",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let cli =
            Cli::try_parse_from(["nexus", "doc"].into_iter().chain(args.iter().copied())).unwrap();
        let Some(Command::Doc { cmd }) = cli.command else {
            panic!("expected doc command")
        };
        doc_files(cmd, &temp)
    };
    run(&["import", "report.md"]).unwrap();
    assert!(temp.join("docs/report.doc.json").is_file());
    run(&["import", "report.md"]).unwrap();
    assert!(temp.join("docs/report-2.doc.json").is_file());
    for format in ["docx", "pdf", "md", "html"] {
        run(&["export", "docs/report.doc.json", "--format", format]).unwrap();
        assert!(temp.join(format!("exports/cli-report.{format}")).is_file());
    }
    run(&[
        "import",
        "exports/cli-report.docx",
        "--out",
        "docs/roundtrip.doc.json",
    ])
    .unwrap();
    let error = run(&[
        "import",
        "exports/cli-report.docx",
        "--out",
        "docs/roundtrip.doc.json",
    ])
    .unwrap_err();
    assert!(error.to_string().contains("never overwrite"));
    let custom = temp.join("custom.html");
    run(&[
        "export",
        "docs/report.doc.json",
        "--format",
        "html",
        "--out",
        custom.to_str().unwrap(),
    ])
    .unwrap();
    assert!(custom.is_file());
    assert!(Cli::try_parse_from(["nexus", "doc", "export", "x", "--format", "svg"]).is_err());
    std::fs::remove_dir_all(temp).unwrap();
}
