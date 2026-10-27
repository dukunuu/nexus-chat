use super::*;

#[test]
fn design_cli_import_list_and_plain_export_switches() {
    let temp = std::env::temp_dir().join(format!("nexus-cli-template-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("brand.dotx"),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../core/tests/fixtures/templates/brand.dotx"
        )),
    )
    .unwrap();
    for args in [
        vec!["import", "brand.dotx", "--name", "Corporate"],
        vec!["list"],
    ] {
        let cli = Cli::try_parse_from(["nexus", "design"].into_iter().chain(args)).unwrap();
        let Some(Command::Design { cmd }) = cli.command else {
            panic!("design command");
        };
        design_files(cmd, &temp).unwrap();
    }
    let design = nexus_core::design::load(&temp, "corporate").unwrap();
    assert_eq!(design.name, "Corporate");
    assert_eq!(design.source.unwrap().template, "templates/corporate.dotx");
    for (kind, format) in [("deck", "pptx"), ("doc", "docx")] {
        assert!(
            Cli::try_parse_from([
                "nexus",
                kind,
                "export",
                "file",
                "--format",
                format,
                "--template",
                "none"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "nexus",
                kind,
                "export",
                "file",
                "--format",
                format,
                "--template",
                "unknown"
            ])
            .is_err()
        );
    }
    std::fs::remove_dir_all(temp).unwrap();
}
