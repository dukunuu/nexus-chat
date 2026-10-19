//! Durable, device-owned agent jobs and their isolated headless runs.

use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod approval;
mod runner;
pub mod service;
mod tree;

pub use approval::{JobApprover, JobExecutor, PARKED};
pub use runner::{DaemonLock, daemon, request_run, run_due, run_once, run_with_app};

/// User-editable job parameters. Times are RFC 3339; intervals are minutes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobSpec {
    pub name: String,
    pub prompt: String,
    pub root: String,
    pub mode: String,
    pub trigger_kind: String,
    pub interval_minutes: i64,
    pub run_at: Option<String>,
    pub stop_command: Option<String>,
    pub max_iters: u32,
    pub max_cost_usd: f64,
    pub max_wall_minutes: u32,
    pub notify: String,
}

impl PartialEq for JobSpec {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.prompt == other.prompt
            && self.root == other.root
            && self.mode == other.mode
            && self.trigger_kind == other.trigger_kind
            && self.interval_minutes == other.interval_minutes
            && self.run_at == other.run_at
            && self.stop_command == other.stop_command
            && self.max_iters == other.max_iters
            && self.max_cost_usd.to_bits() == other.max_cost_usd.to_bits()
            && self.max_wall_minutes == other.max_wall_minutes
            && self.notify == other.notify
    }
}

impl Eq for JobSpec {}

impl Default for JobSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            prompt: String::new(),
            root: String::new(),
            mode: "agent".into(),
            trigger_kind: "until_done".into(),
            interval_minutes: 1440,
            run_at: None,
            stop_command: None,
            max_iters: 5,
            max_cost_usd: 1.0,
            max_wall_minutes: 30,
            notify: "always".into(),
        }
    }
}

impl JobSpec {
    /// Validate the form independently of workspace accessibility.
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.name.trim().is_empty(), "name is required");
        ensure!(!self.prompt.trim().is_empty(), "prompt is required");
        ensure!(!self.root.trim().is_empty(), "workspace root is required");
        ensure!(
            matches!(self.mode.as_str(), "agent" | "auto"),
            "mode must be agent or auto"
        );
        ensure!(
            matches!(
                self.trigger_kind.as_str(),
                "interval" | "once" | "until_done"
            ),
            "trigger must be interval, once or until_done"
        );
        ensure!(self.interval_minutes > 0, "interval must be positive");
        if self.trigger_kind == "once" {
            ensure!(
                self.run_at.as_deref().and_then(parse_time).is_some(),
                "run-at must be an RFC 3339 timestamp"
            );
        }
        ensure!(
            self.max_iters > 0 && self.max_wall_minutes > 0,
            "iteration and wall budgets must be positive"
        );
        ensure!(
            self.max_cost_usd.is_finite() && self.max_cost_usd > 0.0,
            "cost budget must be positive and finite"
        );
        ensure!(
            matches!(self.notify.as_str(), "always" | "on_failure"),
            "notify must be always or on_failure"
        );
        Ok(())
    }
}

/// A synced job. A negative enabled value is a replicated soft deletion.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: String,
    pub space_id: String,
    /// Latest run session; run ids are also their transcript session ids.
    pub session_id: String,
    pub device_id: String,
    pub spec: JobSpec,
    pub enabled: i64,
    pub last_run_at: Option<String>,
    pub updated_at: String,
}

/// Persisted run result; `id` also identifies its ordinary transcript session.
#[derive(Clone, Debug, PartialEq)]
pub struct JobRun {
    pub id: String,
    pub job_id: String,
    pub device_id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub iters: u32,
    pub cost_usd: f64,
    pub branch: Option<String>,
    pub summary: String,
    pub updated_at: String,
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// Pure scheduling: only enabled jobs belonging to `device_id` can be due.
/// Until-done is a single bounded run, rather than an unbounded interval.
pub fn due(jobs: &[Job], now: DateTime<Utc>, device_id: &str) -> Vec<Job> {
    jobs.iter()
        .filter(|job| {
            if job.enabled != 1 || job.device_id != device_id {
                return false;
            }
            match job.spec.trigger_kind.as_str() {
                "interval" => job.last_run_at.as_deref().is_none_or(|last| {
                    parse_time(last).is_some_and(|last| {
                        now.signed_duration_since(last).num_minutes() >= job.spec.interval_minutes
                    })
                }),
                "once" => {
                    job.last_run_at.is_none()
                        && job
                            .spec
                            .run_at
                            .as_deref()
                            .and_then(parse_time)
                            .is_some_and(|at| at <= now)
                }
                "until_done" => job.last_run_at.is_none(),
                _ => false,
            }
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
