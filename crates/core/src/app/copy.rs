use super::{App, CopyOption, Popup, code_blocks};

impl App {
    /// Copy arbitrary text to the clipboard and report it in the status line.
    pub fn copy_text(&mut self, text: &str) {
        let msg = crate::input::copy_to_clipboard(&mut self.clipboard, text);
        if !msg.is_empty() {
            self.push_status(msg);
        }
    }

    /// Copy a message's exact original content by its index into `self.messages`
    /// (streaming reply uses index `messages.len()`) — not the on-screen,
    /// wrap-reconstructed text a long-press selects for highlighting.
    pub fn copy_message(&mut self, idx: usize) {
        let text = match self.messages.get(idx) {
            Some(m) if m.role == "assistant" => Some(crate::markdown::to_plain(&m.content)),
            Some(m) => Some(m.content.clone()),
            None if idx == self.messages.len() => self.active_streaming_text().map(str::to_string),
            None => None,
        };
        if let Some(t) = text {
            self.copy_text(&t);
        }
    }

    /// Open the `/copy` menu for the last assistant reply: the whole response
    /// plus one entry per fenced code block.
    pub fn open_copy_menu(&mut self) {
        let opts = {
            let Some(msg) = self.messages.iter().rev().find(|m| m.role == "assistant") else {
                self.push_status("no response to copy");
                return;
            };
            let mut opts = vec![CopyOption {
                label: "Entire response".into(),
                text: crate::markdown::to_plain(&msg.content),
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
            super::clamp_cursor(self.copy_selected, self.copy_options.len(), delta);
    }
}
