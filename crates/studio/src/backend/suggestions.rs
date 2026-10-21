//! Local suggestion history and asynchronous jobs, independent of the window.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail, ensure};
use nexus_core::deck;
use nexus_core::deck::suggest::{Selection, SuggestRequest, SuggestResponse, Suggestion};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Backend, Event, atomic_write, host, read_bounded};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Accepted,
    Rejected,
    Outdated,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    #[serde(flatten)]
    pub suggestion: Suggestion,
    pub status: Status,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct History {
    pub instruction: String,
    pub model: String,
    pub created: u64,
    pub revision: String,
    pub suggestions: Vec<Entry>,
}

pub enum Job {
    HostStatus,
    StartHost,
    Suggest(Box<SuggestRequest>),
}

pub struct Work {
    connection: host::Connection,
    document: Uuid,
    data_dir: PathBuf,
    job: Job,
}

pub struct Completed {
    document: Uuid,
    suggestion: bool,
    result: std::result::Result<Outcome, String>,
}

enum Outcome {
    Host(host::Status),
    Suggestions {
        response: SuggestResponse,
        instruction: String,
    },
}

impl Work {
    pub async fn run(self) -> Completed {
        let suggestion = matches!(self.job, Job::Suggest(_));
        let result = match self.job {
            Job::HostStatus => Ok(Outcome::Host(self.connection.status().await)),
            Job::StartHost => self
                .connection
                .start(&self.data_dir)
                .await
                .map(Outcome::Host),
            Job::Suggest(request) => {
                self.connection
                    .suggest(&request)
                    .await
                    .map(|response| Outcome::Suggestions {
                        response,
                        instruction: request.instruction,
                    })
            }
        };
        Completed {
            document: self.document,
            suggestion,
            result: result.map_err(|e| e.to_string()),
        }
    }
}

impl Backend {
    pub fn configure_host(&mut self, connection: host::Connection) {
        self.host = Some(connection);
    }

    pub fn take_work(&mut self) -> Option<Work> {
        self.work.take()
    }

    pub fn complete(&mut self, result: Completed) -> Event {
        if result.suggestion {
            self.suggesting = false;
        }
        let outcome = (|| -> Result<Event> {
            ensure!(
                result.document == self.document,
                "Deck changed while contacting host"
            );
            match result.result.map_err(anyhow::Error::msg)? {
                Outcome::Host(status) => Ok(Event::HostStatus { status }),
                Outcome::Suggestions {
                    response,
                    instruction,
                } => {
                    let model = response.model.clone();
                    let notes = response.notes;
                    self.history.push(History {
                        instruction,
                        model: model.clone(),
                        revision: response.revision,
                        created: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
                        suggestions: response
                            .suggestions
                            .into_iter()
                            .map(|suggestion| Entry {
                                suggestion,
                                status: Status::Pending,
                            })
                            .collect(),
                    });
                    if self.history.len() > 50 {
                        self.history.drain(..self.history.len() - 50);
                    }
                    self.recheck();
                    self.persist_history()?;
                    Ok(Event::Suggestions {
                        history: self.history.clone(),
                        model,
                        notes,
                    })
                }
            }
        })();
        outcome.unwrap_or_else(|e| Event::Error {
            message: e.to_string(),
        })
    }

    pub(super) fn request_host(&mut self, job: Job) -> Result<Event> {
        let connection = self
            .host
            .clone()
            .context("Host connection is not configured")?;
        let suggesting = matches!(job, Job::Suggest(_));
        if suggesting {
            ensure!(!self.suggesting, "Already suggesting");
            self.suggesting = true;
        }
        self.work = Some(Work {
            connection,
            document: self.document,
            data_dir: self.recent.parent().context("missing data folder")?.into(),
            job,
        });
        Ok(if suggesting {
            Event::Suggesting {}
        } else {
            Event::HostChecking {}
        })
    }

    pub(super) fn suggest(
        &mut self,
        instruction: String,
        selection: Selection,
        model: Option<String>,
    ) -> Result<Event> {
        let req = SuggestRequest {
            deck: self.deck.clone(),
            revision: self.deck.revision(),
            selection,
            instruction,
            model,
        };
        ensure!(req.check().is_ok(), "Invalid suggestion request");
        self.request_host(Job::Suggest(Box::new(req)))
    }

    pub(super) fn preview(&self, id: &str) -> Result<Event> {
        let entry = self.entry(id).context("Unknown suggestion")?;
        ensure!(entry.status == Status::Pending, "outdated");
        let deck = deck::apply(&self.deck, &entry.suggestion.ops).context("outdated")?;
        Ok(Event::Preview { deck })
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.history
            .iter()
            .flat_map(|h| &h.suggestions)
            .find(|e| e.suggestion.id == id)
    }

    fn set_status(&mut self, id: &str, status: Status) -> Result<()> {
        let entry = self
            .history
            .iter_mut()
            .flat_map(|h| &mut h.suggestions)
            .find(|e| e.suggestion.id == id)
            .context("Unknown suggestion")?;
        entry.status = status;
        Ok(())
    }

    pub(super) fn accept(&mut self, id: &str) -> Result<()> {
        let entry = self.entry(id).context("Unknown suggestion")?;
        ensure!(entry.status == Status::Pending, "outdated");
        if let Ok(next) = deck::apply(&self.deck, &entry.suggestion.ops) {
            self.set_status(id, Status::Accepted)?;
            self.replace(next);
            self.recheck();
            self.persist_history()
        } else {
            self.set_status(id, Status::Outdated)?;
            self.persist_history()?;
            bail!("outdated")
        }
    }

    pub(super) fn reject(&mut self, id: &str) -> Result<()> {
        ensure!(
            self.entry(id).is_some_and(|e| e.status == Status::Pending),
            "Suggestion is not pending"
        );
        self.set_status(id, Status::Rejected)?;
        self.persist_history()
    }

    pub(super) fn accept_all(&mut self) -> Result<()> {
        let ids: Vec<_> = self
            .history
            .iter()
            .flat_map(|h| &h.suggestions)
            .filter(|e| e.status == Status::Pending)
            .map(|e| e.suggestion.id.clone())
            .collect();
        for id in ids {
            // Earlier acceptances may have invalidated later proposals.
            if self.entry(&id).is_some_and(|e| e.status == Status::Pending) {
                self.accept(&id)?;
            }
        }
        Ok(())
    }

    pub(super) fn recheck(&mut self) {
        for entry in self.history.iter_mut().flat_map(|h| &mut h.suggestions) {
            if entry.status == Status::Pending
                && deck::apply(&self.deck, &entry.suggestion.ops).is_err()
            {
                entry.status = Status::Outdated;
            }
        }
    }

    fn history_path(&self) -> PathBuf {
        let mut path = self.path.as_os_str().to_os_string();
        path.push(".suggestions.json");
        path.into()
    }

    pub(super) fn load_history(&mut self) -> Result<()> {
        let path = self.history_path();
        self.history = if path.exists() {
            serde_json::from_slice(&read_bounded(&path, 128 * 1024 * 1024)?)?
        } else {
            Vec::new()
        };
        if self.history.len() > 50 {
            self.history.drain(..self.history.len() - 50);
        }
        self.recheck();
        if path.exists() {
            self.persist_history()?;
        }
        Ok(())
    }

    pub(super) fn persist_history(&self) -> Result<()> {
        if self.history.is_empty() {
            return Ok(());
        }
        atomic_write(
            &self.history_path(),
            &serde_json::to_vec_pretty(&self.history)?,
        )
    }
}

#[cfg(test)]
mod tests;
