//! Export destinations and shell-free reveal commands.

use super::{Backend, Event, Path, Result, asset_root, deck, ensure};
use std::ffi::OsString;

impl Backend {
    pub(super) fn export(
        &mut self,
        format: deck::export::Format,
        path: Option<&Path>,
    ) -> Result<Event> {
        let output = deck::export::export(
            &self.deck,
            Some(&asset_root(&self.path)?),
            format,
            deck::export::Options::default(),
        )?;
        let path = deck::export::write_export(&output, &self.files_dir, path)?;
        self.exports.push(path.clone());
        Ok(Event::Exported {
            path,
            warnings: output.warnings,
        })
    }

    pub(super) fn reveal(&self, path: &Path) -> Result<Event> {
        ensure!(
            self.exports.iter().any(|p| p == path),
            "only this window's exported files can be revealed"
        );
        let (program, args) = reveal_argv(path, std::env::consts::OS);
        let mut child = std::process::Command::new(program).args(args).spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(Event::Revealed {})
    }
}

fn reveal_argv(path: &Path, os: &str) -> (&'static str, Vec<OsString>) {
    match os {
        "macos" => ("open", vec!["-R".into(), path.as_os_str().into()]),
        "windows" => {
            let mut arg = OsString::from("/select,");
            arg.push(path.as_os_str());
            ("explorer", vec![arg])
        }
        _ => (
            "xdg-open",
            vec![path.parent().expect("export parent").as_os_str().into()],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_defaults_uniquifies_and_returns_warnings_without_changing_deck() {
        let temp = std::env::temp_dir().join(format!("nexus-export-{}", uuid::Uuid::new_v4()));
        let mut backend = Backend::start(None, &temp.join("files"), &temp.join("data")).unwrap();
        let before = backend.deck.clone();
        let mut paths = Vec::new();
        for _ in 0..2 {
            let event = backend.dispatch(r#"{"command":"Export","format":"pptx"}"#);
            let Event::Exported { path, warnings } = event else {
                panic!("{event:?}");
            };
            assert_eq!(
                path.parent().unwrap(),
                backend.files_dir.canonicalize().unwrap().join("exports")
            );
            assert!(path.is_file());
            assert!(warnings.iter().any(|w| w.contains("chart")));
            paths.push(path);
        }
        assert_ne!(paths[0], paths[1]);
        assert!(paths[1].to_string_lossy().ends_with(" (2).pptx"));
        assert_eq!(backend.deck, before);
        assert!(backend.undo.is_empty());
        assert!(matches!(
            backend.dispatch(r#"{"command":"Export","format":"json","path":"relative.json"}"#),
            Event::Error { .. }
        ));
        assert!(matches!(
            backend.dispatch(r#"{"command":"Reveal","path":"/not-an-export"}"#),
            Event::Error { .. }
        ));
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn reveal_argv_preserves_paths_without_shell_interpretation() {
        let path = Path::new("/tmp/a ; $(touch nope).pptx");
        assert_eq!(
            reveal_argv(path, "macos"),
            ("open", vec![OsString::from("-R"), path.as_os_str().into()])
        );
        assert_eq!(
            reveal_argv(path, "linux"),
            ("xdg-open", vec![OsString::from("/tmp")])
        );
        assert_eq!(
            reveal_argv(path, "windows"),
            (
                "explorer",
                vec![OsString::from("/select,/tmp/a ; $(touch nope).pptx")]
            )
        );
    }
}
