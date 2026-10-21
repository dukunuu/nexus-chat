//! Asynchronous host jobs, independent of the window.

use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use uuid::Uuid;

use super::{Backend, Event, host};

pub enum Job {
    HostStatus,
    StartHost,
    Feedback {
        request: Box<nexus_core::deck::feedback::Request>,
        thread: Option<String>,
    },
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
    Feedback {
        request: Box<nexus_core::deck::feedback::Request>,
        thread: Option<String>,
        response: Box<nexus_core::deck::feedback::Response>,
    },
}

impl Work {
    pub async fn run(self) -> Completed {
        let suggestion = matches!(self.job, Job::Feedback { .. });
        let result = match self.job {
            Job::HostStatus => Ok(Outcome::Host(self.connection.status().await)),
            Job::StartHost => self
                .connection
                .start(&self.data_dir)
                .await
                .map(Outcome::Host),
            Job::Feedback { request, thread } => {
                self.connection
                    .feedback(&request)
                    .await
                    .map(|response| Outcome::Feedback {
                        request,
                        thread,
                        response: Box::new(response),
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
                Outcome::Feedback {
                    request,
                    thread,
                    response,
                } => self.receive_feedback(&request, thread, *response),
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
        let suggesting = matches!(job, Job::Feedback { .. });
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
}

#[cfg(test)]
mod tests;
