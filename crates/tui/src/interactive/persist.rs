//! What outlives a frame: saved panel state, panels pinned as mini-apps, and
//! the per-run trust list for `@OpenUrl`. The domain half (storage, sync,
//! commands) lives in `nexus_core::panels`; this half decides when to save,
//! how to show a pinned app, and what the confirmation line says.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crossterm::event::KeyCode;
use nexus_core::app::Popup;
use nexus_core::interactive::{Document, PanelSnapshot, Session, panel_key};
use unicode_width::UnicodeWidthStr;

use super::{Entry, Key, Placement, Widgets};
use crate::app_view::AppView;

/// Owner index of pinned-app keys, far above any transcript position.
const APP_OWNER: usize = 1 << 40;

type Opener = Box<dyn FnMut(&str) -> anyhow::Result<()> + Send>;

/// Where a panel's saved state goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// A message panel, by its conversation-local panel key.
    Panel(String),
    /// A pinned app, by id.
    App(String),
}

pub(super) struct Meta {
    target: Target,
    /// Session revision already accounted for.
    rev: u64,
    /// The record last read from or written to storage.
    saved: Option<String>,
}

/// Persistence bookkeeping beside the widget entries.
#[derive(Default)]
pub(super) struct Store {
    /// Conversation the loaded records belong to.
    session_id: Option<String>,
    /// That conversation's saved records, by panel key.
    records: HashMap<String, String>,
    occurrence: HashMap<Key, usize>,
    meta: HashMap<Key, Meta>,
    /// Pinned app ids by key ordinal.
    apps: Vec<String>,
    /// Hosts the person allowed for the rest of this run.
    trusted: HashSet<String>,
    /// Replaces the OS opener (tests).
    opener: Option<Opener>,
    pub(super) pending_pin: bool,
}

impl Store {
    /// What survives a viewport reset: the loaded records and the trust list.
    pub(super) fn retained(self) -> Self {
        Self {
            session_id: self.session_id,
            records: self.records,
            trusted: self.trusted,
            opener: self.opener,
            ..Self::default()
        }
    }
}

impl Widgets {
    /// Number each panel among identical ones so equal sources stay distinct.
    pub(super) fn index_occurrences(&mut self, placements: &[Placement]) {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        self.store.occurrence.clear();
        for placement in placements.iter().filter(|p| p.complete) {
            let count = seen.entry(placement.source.as_str()).or_default();
            self.store.occurrence.insert(placement.key(), *count);
            *count += 1;
        }
    }

    /// Bring a freshly built message panel back to its saved state.
    pub(super) fn restore_message_panel(&mut self, key: &Key, session: &mut Session) {
        if session.document().is_static() {
            return;
        }
        let panel = panel_key(
            &key.source,
            self.store.occurrence.get(key).copied().unwrap_or(0),
        );
        let saved = self.store.records.get(&panel).cloned();
        if let Some(snapshot) = saved
            .as_deref()
            .and_then(|record| PanelSnapshot::decode(record).ok())
        {
            // A record the program no longer accepts only costs the saved state.
            let _ = session.restore(&snapshot);
        }
        self.store.meta.insert(
            key.clone(),
            Meta {
                target: Target::Panel(panel),
                rev: session.revision(),
                saved,
            },
        );
    }

    pub(super) fn is_app(&self, key: &Key) -> bool {
        matches!(
            self.store.meta.get(key),
            Some(Meta {
                target: Target::App(_),
                ..
            })
        )
    }

    /// Whether the keyboard owner is a pinned app.
    pub(crate) fn focused_app(&self) -> bool {
        self.focused.as_ref().is_some_and(|key| self.is_app(key))
    }

    /// Keys of focused pinned apps, which are not in the transcript's visible
    /// set but still run their data calls.
    pub(super) fn app_keys(&self) -> Vec<Key> {
        self.focused
            .iter()
            .filter(|key| self.is_app(key))
            .cloned()
            .collect()
    }

    /// Records that changed since they were last stored.
    pub(super) fn take_dirty(&mut self) -> Vec<(Target, String)> {
        let mut dirty = Vec::new();
        for (key, meta) in &mut self.store.meta {
            let Some(Ok(session)) = self.entries.get(key).map(|entry| &entry.session) else {
                continue;
            };
            if session.revision() == meta.rev {
                continue;
            }
            meta.rev = session.revision();
            let Some(record) = session.snapshot().encode() else {
                continue;
            };
            if meta.saved.as_deref() == Some(record.as_str()) {
                continue;
            }
            meta.saved = Some(record.clone());
            dirty.push((meta.target.clone(), record));
        }
        dirty
    }

    fn app_slot(&mut self, id: &str) -> usize {
        self.store
            .apps
            .iter()
            .position(|known| known == id)
            .unwrap_or_else(|| {
                self.store.apps.push(id.to_string());
                self.store.apps.len() - 1
            })
    }

    pub(super) fn pending_url_focused(&self) -> bool {
        self.focused
            .as_ref()
            .and_then(|key| self.entries.get(key))
            .and_then(|entry| entry.session.as_ref().ok())
            .is_some_and(|session| session.pending_url().is_some())
    }

    /// Enter opens, `a` opens and remembers the host for this run, Esc cancels.
    pub(super) fn resolve_url(&mut self, code: KeyCode) {
        let Some(Ok(session)) = self
            .focused
            .as_ref()
            .and_then(|key| self.entries.get_mut(key))
            .map(|entry| &mut entry.session)
        else {
            return;
        };
        let host = session.pending_host();
        let opener = &mut self.store.opener;
        let status = match session.confirm_url(code != KeyCode::Esc, |url| open_url(opener, url)) {
            Ok(Some(status)) => match host.filter(|_| code == KeyCode::Char('a')) {
                Some(host) => {
                    self.store.trusted.insert(host.clone());
                    format!("{status} · {host} allowed until you quit")
                }
                None => status,
            },
            Ok(None) => "URL opening cancelled".into(),
            Err(error) => format!("URL opening failed: {error}"),
        };
        self.pending_status = Some(status);
    }

    /// Open a freshly requested URL at once when its host was already allowed.
    pub(super) fn open_trusted(&mut self) {
        if self.store.trusted.is_empty() {
            return;
        }
        let Some(Ok(session)) = self
            .focused
            .as_ref()
            .and_then(|key| self.entries.get_mut(key))
            .map(|entry| &mut entry.session)
        else {
            return;
        };
        let Some(host) = session
            .pending_host()
            .filter(|host| self.store.trusted.contains(host))
        else {
            return;
        };
        let opener = &mut self.store.opener;
        self.pending_status = Some(
            match session.confirm_url(true, |url| open_url(opener, url)) {
                Ok(Some(status)) => format!("{status} · {host} is allowed"),
                Ok(None) => return,
                Err(error) => format!("URL opening failed: {error}"),
            },
        );
    }

    /// Route URL opening elsewhere than the OS (tests).
    #[cfg(test)]
    pub(crate) fn set_opener(&mut self, opener: Opener) {
        self.store.opener = Some(opener);
    }

    /// Take a pending `p` press.
    pub(crate) fn take_pin_request(&mut self) -> bool {
        std::mem::take(&mut self.store.pending_pin)
    }
}

fn open_url(opener: &mut Option<Opener>, url: &str) -> anyhow::Result<()> {
    match opener {
        Some(opener) => opener(url),
        None => open::that_detached(url).map_err(Into::into),
    }
}

/// The confirmation line. The whole host is always visible: on a narrow card
/// the hints give way first, then the path.
pub(super) fn url_prompt(url: &str, width: usize) -> String {
    let host = nexus_core::interactive::url_host(url).unwrap_or_default();
    let origin = format!("https://{host}");
    let room = width.saturating_sub(6);
    let tails = [
        format!("? Enter confirm · a always {host} · Esc cancel"),
        format!("? Enter confirm · a always {host}"),
        "? Enter · a always · Esc".to_string(),
        "? Enter confirm".to_string(),
    ];
    let head = "Open ".width() + origin.width();
    let tail = tails
        .iter()
        .find(|tail| head + tail.width() <= room)
        .unwrap_or(&tails[3]);
    let budget = room.saturating_sub(head + tail.width());
    let verbatim = url.to_ascii_lowercase().starts_with(&origin);
    let shown = if !verbatim {
        // Show the parsed (punycode) host, never a lookalike spelling.
        origin
    } else if url.width() <= origin.width() + budget {
        url.to_string()
    } else {
        // Keep the origin, then as much of the path as fits, then an ellipsis.
        let mut shown: String = url.chars().take(origin.chars().count()).collect();
        for ch in url.chars().skip(origin.chars().count()) {
            if shown.width() + ch.len_utf8().min(1) + 1 > origin.width() + budget {
                break;
            }
            shown.push(ch);
        }
        shown.push('…');
        shown
    };
    format!("Open {shown}{tail}")
}

/// Load the saved records of the conversation now on screen, flushing the
/// previous one first.
pub(crate) fn sync_saved(app: &mut AppView) {
    let current = app.session.as_ref().map(|session| session.id.clone());
    if app.interactive.store.session_id == current {
        return;
    }
    flush(app);
    let records = match &current {
        Some(id) if !app.core.incognito => app.core.panel_states(id).unwrap_or_default(),
        _ => HashMap::new(),
    };
    app.interactive.store.session_id = current;
    app.interactive.store.records = records;
}

/// Store every panel whose state changed since the last flush.
pub(crate) fn flush(app: &mut AppView) {
    let dirty = app.interactive.take_dirty();
    if dirty.is_empty() {
        return;
    }
    let session_id = app.interactive.store.session_id.clone();
    let mut failure = None;
    for (target, record) in dirty {
        let result = match &target {
            Target::Panel(panel) => match &session_id {
                Some(id) => {
                    let result = app.core.save_panel_state(id, panel, &record);
                    app.interactive
                        .store
                        .records
                        .insert(panel.clone(), record.clone());
                    result
                }
                None => Ok(()),
            },
            Target::App(id) => app.core.save_pinned_state(id, &record),
        };
        if let Err(error) = result {
            failure = Some(error);
        }
    }
    if let Some(error) = failure {
        app.push_status(format!("saving panel state failed: {error}"));
    }
}

/// Pin the focused panel (or the newest visible one) as a mini-app.
pub(crate) fn pin_focused(app: &mut AppView, name: Option<&str>) {
    let widgets = &app.interactive;
    let Some(key) = widgets
        .focused
        .clone()
        .or_else(|| widgets.visible.last().cloned())
    else {
        app.push_status("no interactive panel to pin — press F2 to focus one");
        return;
    };
    if widgets.is_app(&key) {
        app.push_status("this panel is already a pinned app");
        return;
    }
    let Some(Ok(session)) = widgets.entries.get(&key).map(|entry| &entry.session) else {
        app.push_status("that panel is not available to pin");
        return;
    };
    let program = session
        .program()
        .map_or_else(|| key.source.clone(), str::to_string);
    let snapshot = session.snapshot();
    if let Err(error) = app.core.pin_panel(name, &program, &snapshot) {
        app.push_status(format!("pin failed: {error}"));
    }
}

/// Show a pinned app full size, with the state it was left in. Returns false
/// when no pinned app has that name or id.
pub(crate) fn open_pinned(app: &mut AppView, reference: &str) -> bool {
    if app.core.incognito {
        return false;
    }
    flush(app);
    let pinned = match app.core.pinned_panel(reference) {
        Ok(Some(pinned)) => pinned,
        Ok(None) => return false,
        Err(error) => {
            app.push_status(format!("pinned apps unavailable: {error}"));
            return false;
        }
    };
    let mut session = match Document::parse(&pinned.program).and_then(Session::new) {
        Ok(session) => session,
        Err(error) => {
            app.push_status(format!("{} cannot open: {error}", pinned.name));
            return true;
        }
    };
    if let Ok(snapshot) = PanelSnapshot::decode(&pinned.record)
        && let Err(error) = session.restore(&snapshot)
    {
        app.push_status(format!("saved state not restored: {error}"));
    }
    // Opening an app is explicit use: free reads may refresh at once, while
    // paid calls still wait for the Load button.
    session.interact_data(Instant::now());
    let widgets = &mut app.interactive;
    let key = Key {
        owner: APP_OWNER,
        ordinal: widgets.app_slot(&pinned.id),
        source: pinned.program.clone(),
    };
    widgets.store.meta.insert(
        key.clone(),
        Meta {
            target: Target::App(pinned.id.clone()),
            rev: session.revision(),
            saved: Some(pinned.record.clone()),
        },
    );
    widgets.entries.insert(
        key.clone(),
        Entry {
            session: Ok(session),
            focus: 0,
            error: String::new(),
            scroll: 0,
        },
    );
    widgets.focused = Some(key);
    widgets.expanded = true;
    widgets.pinned = false;
    app.popup = Popup::InteractiveUi;
    app.sel.clear();
    app.push_status(format!(
        "{} · Esc closes · changes save automatically",
        pinned.name
    ));
    true
}
