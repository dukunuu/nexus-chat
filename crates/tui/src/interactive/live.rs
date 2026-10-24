//! View-side background receiver; no new domain events or work during drawing.

use super::{AppView, Key, Session, Widgets, review};
use nexus_core::interactive::{live, panel_id};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::oneshot;

pub(super) struct Running {
    key: Key,
    call: live::Call,
    receiver: oneshot::Receiver<live::Outcome>,
    task: tokio::task::JoinHandle<()>,
    session_id: String,
    incognito: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Widgets {
    pub(crate) fn data_pending(&self) -> bool {
        self.data_running.is_some()
            || self
                .entries
                .values()
                .any(|entry| entry.session.as_ref().is_ok_and(Session::has_data))
    }

    pub(crate) fn window_focus(&mut self, focused: bool) {
        self.window_unfocused = !focused;
    }
}

pub(crate) fn tick(app: &mut AppView) {
    let now = Instant::now();
    let completion = app.interactive.data_running.as_mut().and_then(|running| {
        match running.receiver.try_recv() {
            Ok(outcome) => Some(Ok(outcome)),
            Err(oneshot::error::TryRecvError::Closed) => Some(Err(())),
            Err(oneshot::error::TryRecvError::Empty) => None,
        }
    });
    if let Some(completion) = completion {
        let running = app.interactive.data_running.take().expect("completed call");
        match completion {
            Ok(outcome) => {
                let panel = panel_id(running.key.owner, running.key.ordinal);
                if let Err(error) = app.core.record_panel_call(
                    &running.session_id,
                    running.incognito,
                    &panel,
                    &running.call,
                    &outcome,
                ) {
                    app.push_status(format!("panel log failed: {error}"));
                }
                let viewing = app
                    .session
                    .as_ref()
                    .is_some_and(|session| session.id == running.session_id);
                let message = app
                    .interactive
                    .entries
                    .get_mut(&running.key)
                    .filter(|_| viewing)
                    .and_then(|entry| match &mut entry.session {
                        Ok(session) => match session.finish_data_call(&running.call, &outcome, now)
                        {
                            Ok(message) => message,
                            Err(error) => {
                                entry.error = error.to_string();
                                None
                            }
                        },
                        Err(_) => None,
                    });
                // Deferred mutation sends still need review and never replace a draft.
                review(app, message);
            }
            Err(()) => app.push_status("panel tool task stopped"),
        }
    }
    if app.interactive.data_running.is_some()
        || app.is_streaming()
        || app.interactive.window_unfocused
    {
        return;
    }
    let Some(session_id) = app.session.as_ref().map(|session| session.id.clone()) else {
        return;
    };
    let mut keys = app.interactive.visible.clone();
    keys.extend(app.interactive.app_keys());
    for key in keys {
        let count = app
            .interactive
            .data_calls
            .entry(session_id.clone())
            .or_default();
        let Some(entry) = app.interactive.entries.get_mut(&key) else {
            continue;
        };
        let Ok(session) = &mut entry.session else {
            continue;
        };
        let call = match session.next_data_call(now, count) {
            Ok(Some(call)) => call,
            Ok(None) => continue,
            Err(error) => {
                entry.error = error.to_string();
                continue;
            }
        };
        let executor = Arc::clone(&app.toolbox);
        let approver = app.panel_approver.clone();
        let scrubber = app.core.panel_scrubber();
        let (sender, receiver) = oneshot::channel();
        let work = call.clone();
        let panel = panel_id(key.owner, key.ordinal);
        let task = tokio::spawn(async move {
            let outcome = live::execute(&work, &panel, executor, approver, &scrubber).await;
            let _ = sender.send(outcome);
        });
        app.interactive.data_running = Some(Running {
            key,
            call,
            receiver,
            task,
            session_id,
            incognito: app.incognito,
        });
        break;
    }
}
