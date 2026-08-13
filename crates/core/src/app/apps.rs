//! The `/apps` popup's domain half: app URLs and file counts. The popup
//! flow (browse/edit/delete state) lives in the TUI view layer.

use super::App;

impl App {
    /// An app's live URL, when the server is running.
    pub fn app_url(&self, name: &str) -> Option<String> {
        let s = self.app_server.as_ref()?;
        let uuid = s.registry().resolve(&self.active_space.name, name)?;
        Some(s.app_url(&uuid))
    }

    /// How many files an app holds (recursive; `node_modules` counted as one
    /// "deps" marker would be noise, so it's skipped entirely).
    pub fn app_file_count(&self, name: &str) -> usize {
        fn count(dir: &std::path::Path) -> usize {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return 0;
            };
            rd.flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        if e.file_name() == "node_modules" {
                            0
                        } else {
                            count(&p)
                        }
                    } else {
                        1
                    }
                })
                .sum()
        }
        count(&self.space.apps_dir(&self.active_space.name).join(name))
    }
}
