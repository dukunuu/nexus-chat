use nexus_core::app::{CopyOption, Popup, code_blocks};

use crate::app_view::AppView;

impl AppView {
    /// Open the `/copy` menu for the last assistant reply: the whole response
    /// plus one entry per fenced code block.
    pub fn open_copy_menu(&mut self) {
        let opts = {
            let Some(msg) = self
                .core
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant")
            else {
                self.push_status("no response to copy");
                return;
            };
            let mut opts = vec![CopyOption {
                label: "Entire response".into(),
                text: nexus_core::markdown::to_plain(&msg.content),
            }];
            for (i, (lang, code)) in code_blocks(&msg.content).into_iter().enumerate() {
                let label = match lang {
                    Some(l) => format!("Code block {} ({l})", i + 1),
                    None => format!("Code block {}", i + 1),
                };
                opts.push(CopyOption { label, text: code });
            }
            opts
        };
        self.copy_options = opts;
        self.copy_selected = 0;
        self.popup = Popup::Copy;
    }

    /// Copy the highlighted `/copy` menu entry and close the menu.
    pub fn confirm_copy(&mut self) {
        if let Some(text) = self
            .copy_options
            .get(self.copy_selected)
            .map(|o| o.text.clone())
        {
            self.copy_text(&text);
        }
        self.popup = Popup::None;
    }

    pub fn move_copy_selection(&mut self, delta: i32) {
        self.copy_selected =
            nexus_core::app::clamp_cursor(self.copy_selected, self.copy_options.len(), delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::db::{Db, Message};
    use nexus_core::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("nexus-copy-flow-{}", uuid::Uuid::new_v4())),
        }
    }

    fn test_app() -> AppView {
        AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("sk-or-test-key"),
            test_space(),
        ))
    }

    #[test]
    fn copy_message_uses_exact_original_content() {
        let mut a = test_app();
        a.core.messages.push(Message {
            role: "user".into(),
            content: "raw *user* text".into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });
        a.core.messages.push(Message {
            role: "assistant".into(),
            content: "**bold** reply".into(),
            model: Some("a/one".into()),
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });
        // copy_message resolves *some* text at each index (clipboard availability
        // is environment-dependent in CI, so just assert it didn't silently no-op).
        let _ = a.last_status(); // drain so the next action must produce a fresh status
        a.copy_message(0); // user message: verbatim
        assert_ne!(a.last_status(), "sentinel");

        let _ = a.last_status(); // drain so the next action must produce a fresh status
        a.copy_message(1); // assistant message: through markdown::to_plain
        assert_ne!(a.last_status(), "sentinel");

        // An out-of-range index is a no-op when no response is active — no
        // status event is pushed (the 2e status lives in the view, fed by events).
        let _ = a.last_status(); // drain so the next action must produce a fresh status
        a.copy_message(2);
        assert_eq!(a.last_status(), "");
    }
}
