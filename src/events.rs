use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use futures_util::StreamExt;
use ratatui::DefaultTerminal;
use ratatui::layout::{Position, Rect};

use tui_textarea::CursorMove;

use crate::app::{App, AppEvent, ModelPanel, MouseTarget, Popup};
use crate::provider::StreamEvent;
use crate::ui;

/// Ctrl+E/Ctrl+K in the space picker edit that space's instructions/memory
/// file. Returns the path to open, if the key matches and one resolves.
fn edit_file_target(app: &App, key: &KeyEvent) -> Option<std::path::PathBuf> {
    if !(app.popup == Popup::Space
        && app.space_mode == crate::app::SpaceMode::Browse
        && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        return None;
    }
    match key.code {
        KeyCode::Char('e') => app.instructions_path_for_selected(),
        KeyCode::Char('k') => app.memory_path_for_selected(),
        _ => None,
    }
}

/// Ctrl+E in the skills popup opens the highlighted skill's SKILL.md.
fn skill_edit_target(app: &App, key: &KeyEvent) -> Option<std::path::PathBuf> {
    if !(app.popup == Popup::Skills
        && app.skills_mode == crate::app::SkillsMode::Browse
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('e'))
    {
        return None;
    }
    app.skill_edit_path_for_selected()
}

/// Ctrl+E in the settings popup opens the app's base system prompt.
fn system_prompt_edit_target(app: &App, key: &KeyEvent) -> Option<std::path::PathBuf> {
    if !(app.popup == Popup::Settings
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('e'))
    {
        return None;
    }
    crate::config::system_prompt_path().ok()
}

pub async fn run(mut app: App, terminal: &mut DefaultTerminal) -> Result<()> {
    let mut reader = EventStream::new();
    // Cheap poll for an omarchy theme switch (symlink target change) so a
    // `omarchy theme set` while nexus-chat is running takes effect live.
    let mut theme_poll = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        terminal.draw(|f| ui::render(f, &mut app))?;
        if app.should_quit {
            break;
        }

        // Animate the thinking spinner only while a response streams.
        let streaming = app.is_streaming();
        let long_deadline = app.sel.deadline();
        let welcome = app.is_welcome();
        tokio::select! {
            maybe = reader.next() => match maybe {
                Some(Ok(Event::Key(k))) if k.kind == KeyEventKind::Press => {
                    if let Some(path) = edit_file_target(&app, &k) {
                        edit_in_external_editor(terminal, &path)?;
                    } else if let Some(path) = skill_edit_target(&app, &k) {
                        edit_in_external_editor(terminal, &path)?;
                        app.reload_skills();
                    } else if let Some(path) = system_prompt_edit_target(&app, &k) {
                        edit_in_external_editor(terminal, &path)?;
                        app.reload_base_system_prompt();
                    } else if app.popup == Popup::Context && k.code == KeyCode::Char('v') {
                        match app.compact_summary_path() {
                            Some(path) => {
                                edit_in_external_editor(terminal, &path)?;
                                app.reload_compact_summary(&path)?;
                            }
                            None => app.status = "session hasn't been compacted yet".to_string(),
                        }
                    } else {
                        handle_key(&mut app, k)?;
                        // /edit queued an app file — open it now (this loop
                        // owns the terminal, run_command doesn't).
                        if let Some(edit) = app.pending_editor.take() {
                            match edit {
                                crate::app::PendingEditor::AppFile(path) => {
                                    if let Err(e) = edit_in_external_editor(terminal, &path) {
                                        app.status = format!("editor failed: {e}");
                                    }
                                }
                                crate::app::PendingEditor::Persona(path) => {
                                    match edit_in_external_editor(terminal, &path) {
                                        Ok(()) => app.apply_swarm_persona_editor(&path)?,
                                        Err(e) => app.status = format!("editor failed: {e}"),
                                    }
                                }
                                crate::app::PendingEditor::ResearchPlan(path) => {
                                    match edit_in_external_editor(terminal, &path) {
                                        Ok(()) => app.apply_research_plan_editor(&path)?,
                                        Err(e) => app.status = format!("editor failed: {e}"),
                                    }
                                }
                                crate::app::PendingEditor::ScriptFile(path) => {
                                    if let Err(e) = edit_in_external_editor(terminal, &path) {
                                        app.status = format!("editor failed: {e}");
                                    }
                                    app.refresh_scripts();
                                }
                            }
                        }
                    }
                }
                Some(Ok(Event::Mouse(m))) => {
                    let size = terminal.size()?;
                    handle_mouse(&mut app, m, Rect::new(0, 0, size.width, size.height))?;
                }
                // Terminal-native paste (bracketed paste) — goes to whatever's
                // focused: the composer, or a popup's text field.
                Some(Ok(Event::Paste(text))) => {
                    app.paste(&text);
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            event = app.next_event() => match event {
                AppEvent::Stream(Some(e)) => app.on_stream_event(e)?,
                // Channel closed without a Done sentinel (task ended): finalize.
                AppEvent::Stream(None) => app.on_stream_event(StreamEvent::Done)?,
                AppEvent::Models(r) => app.on_models_result(r),
                AppEvent::Title(t) => app.on_title_result(t),
                AppEvent::Memory(m) => app.on_memory_result(m),
                AppEvent::Compact(c) => app.on_compact_result(c),
                AppEvent::SkillInstall(r) => app.on_skill_install_result(r),
                AppEvent::Described(r) => app.on_described(r),
                AppEvent::Ocr(r) => app.on_ocr_done(r),
                AppEvent::Embed(r) => app.on_embed_done(r),
                AppEvent::OcrPull(r) => app.on_ocr_pull(r),
                AppEvent::Research(r) => app.on_research_done(r),
                AppEvent::ResearchTopic(r) => app.on_research_topic_derived(r),
                AppEvent::Login(r) => app.on_login_result(r),
                AppEvent::Swarm(r) => app.on_swarm_update(r),
            },
            _ = async {
                if streaming {
                    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                } else {
                    std::future::pending::<()>().await
                }
            } => app.tick_spinner(),
            // Long-press (held, unmoved) selects the whole conversation.
            _ = async {
                match long_deadline {
                    Some(d) => tokio::time::sleep(d.saturating_duration_since(std::time::Instant::now())).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                match app.sel.check_long_press() {
                    Some(crate::selection::LongPress::Code(text)) => app.copy_text(text),
                    Some(crate::selection::LongPress::Message(idx)) => app.copy_message(idx),
                    Some(crate::selection::LongPress::Url(url)) => app.copy_text(url),
                    None => {}
                }
            }
            // Tick once a second on the start screen so the clock stays live.
            _ = async {
                if welcome {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                } else {
                    std::future::pending::<()>().await
                }
            } => {}
            _ = theme_poll.tick() => {
                let target = crate::theme::current_link_target();
                if target != app.theme_link {
                    app.theme = crate::theme::load();
                    app.theme_link = target;
                    app.theme_gen = app.theme_gen.wrapping_add(1);
                }
            }
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    // Ctrl+C always quits. Selecting text (mouse drag, in the composer or
    // history) copies it on release, so Ctrl+C doesn't need to double as copy.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && key.code == KeyCode::Char('c')
    {
        app.should_quit = true;
        return Ok(());
    }
    // Ctrl+V pastes into whatever's focused (composer or a popup's text
    // field) — a fallback for terminals that don't send bracketed paste for
    // every popup, or don't send it at all.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('v') {
        app.paste_from_clipboard();
        return Ok(());
    }

    match app.popup {
        Popup::Model => ui::popups::model::handle_key(app, key)?,
        Popup::Session => crate::ui::popups::session::handle_key(app, key)?,
        Popup::Key => crate::ui::popups::key::handle_key(app, key),
        Popup::Settings => ui::popups::settings::handle_key(app, key)?,
        Popup::Copy => crate::ui::popups::copy::handle_key(app, key),
        Popup::Space => crate::ui::popups::space::handle_key(app, key)?,
        Popup::Context => crate::ui::popups::context::handle_key(app, key),
        Popup::Skills => crate::ui::popups::skills::handle_key(app, key),
        Popup::Files => ui::popups::files::handle_key(app, key)?,
        Popup::Apps => ui::popups::apps::handle_key(app, key)?,
        Popup::Watch => ui::popups::watches::handle_key(app, key)?,
        Popup::ResearchLive => ui::popups::research_live::handle_key(app, key)?,
        Popup::Swarm => ui::popups::swarm::handle_key(app, key)?,
        Popup::Login => ui::popups::login::handle_key(app, key)?,

        Popup::None => handle_normal(app, key)?,
    }
    Ok(())
}

/// Suspend the TUI, open `path` in `$EDITOR` (falling back to `vi`), then
/// restore the terminal and force a full redraw (its contents are gone after
/// the editor exits).
fn edit_in_external_editor(terminal: &mut DefaultTerminal, path: &std::path::Path) -> Result<()> {
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste
    );
    ratatui::restore();
    let editor_raw = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor_raw.split_whitespace();
    let editor = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(editor)
        .args(parts)
        .arg(path)
        .status();
    *terminal = ratatui::init();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste
    );
    terminal.clear()?;
    match status {
        Ok(code) if code.success() => Ok(()),
        Ok(code) => {
            // Non-zero exit (vim `:cq`, user abort, etc.) — the terminal is
            // restored but the caller should not consume the edited file.
            Err(anyhow::anyhow!(
                "editor exited with code {}",
                code.code().unwrap_or(-1)
            ))
        }
        Err(e) => Err(anyhow::anyhow!("could not launch editor: {e}")),
    }
}

fn handle_normal(app: &mut App, key: KeyEvent) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Slash-command autocomplete captures navigation while it's showing. Typing
    // (chars/backspace) still falls through so the list keeps filtering.
    if !app.command_matches().is_empty() {
        match key.code {
            KeyCode::Up => {
                app.move_command_selection(-1);
                return Ok(());
            }
            KeyCode::Down => {
                app.move_command_selection(1);
                return Ok(());
            }
            KeyCode::Tab => {
                app.accept_command(false)?;
                return Ok(());
            }
            KeyCode::Enter => {
                app.accept_command(true)?;
                return Ok(());
            }
            KeyCode::Esc => {
                app.set_input("");
                return Ok(());
            }
            _ => {}
        }
    }

    match key.code {
        // Shift+Enter and Ctrl+Enter insert a newline; plain Enter sends.
        KeyCode::Enter if shift || ctrl => app.input.insert_newline(),
        // A pending research plan gate intercepts: 'e' (empty composer)
        // prefills the plan for editing, Enter approves (empty composer) or
        // submits the edit (composer text). Normal typing is untouched.
        KeyCode::Char('e')
            if app.research_plan_gate.is_some() && app.input_text().trim().is_empty() =>
        {
            app.edit_research_plan();
        }
        KeyCode::Enter if app.research_plan_gate.is_some() => {
            let text = app.input_text();
            app.set_input("");
            if text.trim().is_empty() {
                app.approve_research_plan();
            } else {
                app.submit_research_plan_edit(&text);
            }
        }
        KeyCode::Enter => app.submit()?,
        // Paste is handled by the terminal's bracketed paste (Event::Paste).
        // Ctrl+Shift+C copies the composer's selection to the OS clipboard for
        // terminals that forward it; otherwise the terminal's own copy works on
        // a mouse selection. Ctrl+X cuts.
        KeyCode::Char('a') if ctrl => app.input.select_all(),
        KeyCode::Char('c') | KeyCode::Char('C') if ctrl && shift => app.copy_selection(),
        KeyCode::Char('x') if ctrl => app.cut_selection(),
        // Ctrl+R expands/collapses stored reasoning traces (editor's redo is
        // shadowed here — the composer rarely needs it).
        KeyCode::Char('r') if ctrl => app.toggle_reasoning_view()?,
        // Ctrl+T expands/collapses tool-call detail blocks in the transcript.
        KeyCode::Char('t') if ctrl => app.show_tool_detail = !app.show_tool_detail,
        // Ctrl+N toggles incognito mode (no persistence, no apps).
        KeyCode::Char('n') if ctrl => app.toggle_incognito()?,
        // 'o' opens the [n] citation under the current history selection;
        // with no active selection it falls through and just types.
        KeyCode::Char('o') if !ctrl && !shift && app.sel.selected_text().is_some() => {
            app.open_citation_under_selection();
        }
        // 'p' pins, 'x' discards the [n] source under the current selection —
        // same selection→citation resolution as 'o'.
        KeyCode::Char('p') if !ctrl && !shift && app.sel.selected_text().is_some() => {
            app.flag_source_under_selection(Some("pinned"));
        }
        KeyCode::Char('x') if !ctrl && !shift && app.sel.selected_text().is_some() => {
            app.flag_source_under_selection(Some("discarded"));
        }
        // Ctrl+Space opens the live research-activity view (per-searcher
        // reasoning/tool calls) — only while a research job is running.
        KeyCode::Char(' ') if ctrl && app.research_rx.is_some() => app.open_research_live(),
        // Ctrl+G opens the context breakdown (system/memory/conversation/skills).
        // (Not Ctrl+I: that's the same byte as Tab on terminals without the
        // Kitty keyboard protocol, so it'd be unreachable on many of them.)
        KeyCode::Char('g') if ctrl => app.popup = Popup::Context,
        // Ctrl+Backspace deletes the previous word. (Alt+Backspace and Ctrl+W
        // also do this via the editor's default keymap.)
        KeyCode::Backspace if ctrl => {
            app.input.delete_word();
        }
        // Up/Down move the composer cursor within a multi-row message first;
        // only once it's already at the top/bottom row do they scroll history.
        // Shift+Up/Down falls through to the default keymap instead, so it
        // extends the selection rather than being swallowed by this.
        KeyCode::Up if !shift => {
            let before = app.input.cursor();
            app.input.move_cursor(CursorMove::Up);
            if app.input.cursor() == before {
                app.scroll = app.scroll.saturating_add(1).min(app.max_scroll);
            }
        }
        KeyCode::Down if !shift => {
            let before = app.input.cursor();
            app.input.move_cursor(CursorMove::Down);
            if app.input.cursor() == before {
                app.scroll = app.scroll.saturating_sub(1);
            }
        }
        KeyCode::PageUp => app.scroll = app.scroll.saturating_add(10).min(app.max_scroll),
        KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(10),
        // Esc while viewing the streaming session stops the response (partial
        // text is kept); otherwise it clears the composer.
        KeyCode::Esc if app.viewing_stream() => app.stop_stream()?,
        KeyCode::Esc => {
            app.set_input("");
            app.pending_images.clear();
        }
        // Everything else (chars, word-jump, selection, cut/copy/paste, undo)
        // goes to the editor via its default keymap. A keyboard (Shift+arrow)
        // selection has no "release" event like a mouse drag does, so keep
        // the clipboard synced to it live instead of requiring Ctrl+Shift+C.
        _ => {
            app.input.input(key);
            if app.input.is_selecting() {
                app.copy_selection_live();
            }
        }
    }
    Ok(())
}

/// Mouse in the main view (no popup): composer click/drag places the cursor and
/// selects; history click/drag/double/triple selects text; wheel scrolls.
fn handle_input_mouse(app: &mut App, m: MouseEvent) {
    let over_input = app.input_inner.contains(Position::new(m.column, m.row));
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if over_input {
                app.mouse_target = MouseTarget::Input;
                app.sel.clear();
                let count = app.composer_click_down((m.column, m.row));
                composer_jump(app, m);
                match count {
                    2 => app.select_composer_word(),
                    n if n >= 3 => app.select_composer_line(),
                    _ => {
                        app.input.cancel_selection();
                        app.composer_word_anchor = None;
                    }
                }
            } else if let Some(p) = app.sel.pos_at(m.column, m.row) {
                app.mouse_target = MouseTarget::History;
                app.sel.on_down(p);
            } else {
                app.mouse_target = MouseTarget::None;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => match app.mouse_target {
            MouseTarget::Input => match app.composer_click_count {
                2 => {
                    composer_jump(app, m);
                    app.extend_composer_word_selection();
                }
                n if n >= 3 => {
                    composer_jump(app, m);
                    app.extend_composer_line_selection();
                }
                _ => {
                    if !app.input.is_selecting() {
                        app.input.start_selection();
                    }
                    composer_jump(app, m);
                }
            },
            MouseTarget::History => {
                if let Some(p) = app.sel.pos_at(m.column, m.row) {
                    app.sel.on_drag(p);
                }
            }
            MouseTarget::None => {}
        },
        MouseEventKind::Up(MouseButton::Left) => {
            match app.mouse_target {
                MouseTarget::History => {
                    let p = app.sel.pos_at(m.column, m.row);
                    let was_image = p.is_some() && app.open_image_at_line(p.unwrap().0);
                    match app.sel.on_up(p) {
                        Some(crate::selection::Action::Copy(text)) => app.copy_text(text),
                        Some(crate::selection::Action::OpenUrl(url)) => {
                            let _ = open::that_detached(&url);
                            app.status = format!("opened {url}");
                        }
                        None if !was_image && p.is_some() => {
                            // Click without drag on a non-image line: open URLs or start selection.
                        }
                        None => {}
                    }
                }
                // A drag in the composer selects text; releasing copies it
                // immediately, same as releasing a history-pane selection.
                // A plain click (no drag) never starts a selection, so this
                // doesn't fire just from placing the cursor.
                MouseTarget::Input if app.input.is_selecting() => app.copy_selection(),
                MouseTarget::Input | MouseTarget::None => {}
            }
            app.mouse_target = MouseTarget::None;
        }
        // Wheel scrolls the conversation history.
        MouseEventKind::ScrollUp => {
            app.scroll = app.scroll.saturating_add(3).min(app.max_scroll);
        }
        MouseEventKind::ScrollDown => app.scroll = app.scroll.saturating_sub(3),
        _ => {}
    }
}

/// Move the composer cursor to the clicked cell. ponytail: screen row/col mapped
/// straight to data line/char — exact when the composer isn't wrapped/scrolled
/// (tui-textarea keeps its screen<->data map private).
fn composer_jump(app: &mut App, m: MouseEvent) {
    let row = m.row.saturating_sub(app.input_inner.y);
    let col = m.column.saturating_sub(app.input_inner.x);
    app.input.move_cursor(CursorMove::Jump(row, col));
}

/// Route mouse events: composer click/drag when no popup is open, else the
/// model picker (the only interactive popup).
fn handle_mouse(app: &mut App, m: MouseEvent, screen: Rect) -> Result<()> {
    if app.popup == Popup::None {
        handle_input_mouse(app, m);
        return Ok(());
    }
    if app.popup != Popup::Model {
        return Ok(());
    }
    let (fav_outer, avail_outer) = ui::popups::model::model_popup_areas(screen);
    let fav_inner = ui::popups::model::list_inner(fav_outer);
    let avail_inner = ui::popups::model::list_inner(avail_outer);
    let pos = Position::new(m.column, m.row);

    // Which panel is the cursor over?
    let panel = if fav_inner.contains(pos) {
        Some((ModelPanel::Favorites, fav_inner, app.fav_state.offset()))
    } else if avail_inner.contains(pos) {
        Some((ModelPanel::Available, avail_inner, app.avail_state.offset()))
    } else {
        None
    };

    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((p, inner, offset)) = panel {
                let index = offset + (m.row - inner.y) as usize;
                app.pick_model_at(p, index)?; // no-op if index is past the list
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some((p, ..)) = panel {
                app.model_focus = p;
                app.move_model_selection(1);
            }
        }
        MouseEventKind::ScrollUp => {
            if let Some((p, ..)) = panel {
                app.model_focus = p;
                app.move_model_selection(-1);
            }
        }
        _ => {}
    }
    Ok(())
}
