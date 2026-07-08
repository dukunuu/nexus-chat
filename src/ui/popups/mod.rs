pub(crate) mod apps;
pub(crate) mod chrome;
pub(crate) mod context;
pub(crate) mod copy;
pub(crate) mod files;
pub(crate) mod key;
pub(crate) mod login;
pub(crate) mod model;
pub(crate) mod research_live;
pub(crate) mod session;
pub(crate) mod settings;
pub(crate) mod skills;
pub(crate) mod space;
pub(crate) mod swarm;
pub(crate) mod watches;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// Shared state-machine helpers for the session/space/skills popups, which all
// hand-roll the same "Browse list (filter as you type) / Edit text field /
// confirm-delete" shape. Each popup's own `handle_key` still owns *what*
// happens for a given action (e.g. what "Close" clears) — these helpers only
// centralize *which* keys map to which action, since that mapping is
// identical across the 3 (module-doc'd divergences: skills has no rename and
// no Browse-mode text filter; space gates its delete-confirm key on the
// selected space not being the default one — that guard stays in space.rs).

/// Actions available while browsing a popup's list (the default mode).
///
/// `Close`/`MoveUp`/`MoveDown`/`Backspace`/`Filter` are common to all 3
/// popups' Browse mode. `Create` and `Rename` are gated by
/// `classify_browse_key`'s `supports_create`/`supports_rename` flags so a
/// popup that doesn't support one simply never receives it (matching the
/// current behavior where, e.g., skills' Browse arm has no Ctrl+R case at
/// all and falls through to its `_ => {}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BrowseAction {
    Close,
    Filter(char),
    Backspace,
    MoveUp,
    MoveDown,
    Create,
    Rename,
    ConfirmDelete,
}

/// Maps a keypress to a `BrowseAction` for a popup's Browse mode.
///
/// This does NOT handle Enter — session and space each treat Enter as
/// "open/select the highlighted item", which is entirely popup-specific
/// (`confirm_session`/`confirm_space`) and has no skills equivalent, so each
/// `handle_key` still matches `KeyCode::Enter` itself before falling back to
/// this classifier.
pub(super) fn classify_browse_key(
    key: KeyEvent,
    supports_create: bool,
    supports_rename: bool,
) -> Option<BrowseAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => Some(BrowseAction::Close),
        KeyCode::Up => Some(BrowseAction::MoveUp),
        KeyCode::Down => Some(BrowseAction::MoveDown),
        KeyCode::Char('n') if ctrl && supports_create => Some(BrowseAction::Create),
        KeyCode::Char('r') if ctrl && supports_rename => Some(BrowseAction::Rename),
        KeyCode::Char('d') if ctrl => Some(BrowseAction::ConfirmDelete),
        KeyCode::Backspace => Some(BrowseAction::Backspace),
        KeyCode::Char(c) if !ctrl => Some(BrowseAction::Filter(c)),
        _ => None,
    }
}

/// Actions available while editing a text field (rename / create / install
/// prompt). Identical across all 3 popups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditAction {
    Cancel,
    Save,
    Backspace,
    Push(char),
}

pub(super) fn classify_edit_key(key: KeyEvent) -> Option<EditAction> {
    match key.code {
        KeyCode::Esc => Some(EditAction::Cancel),
        KeyCode::Enter => Some(EditAction::Save),
        KeyCode::Backspace => Some(EditAction::Backspace),
        KeyCode::Char(c) => Some(EditAction::Push(c)),
        _ => None,
    }
}

/// Actions available while a delete confirmation is showing. Identical
/// across all 3 popups: Ctrl+D confirms, Esc cancels, everything else is a
/// no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfirmDeleteAction {
    Yes,
    No,
}

pub(super) fn classify_confirm_delete_key(key: KeyEvent) -> Option<ConfirmDeleteAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('d') if ctrl => Some(ConfirmDeleteAction::Yes),
        KeyCode::Esc => Some(ConfirmDeleteAction::No),
        _ => None,
    }
}
