use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::provider::ToolDef;
use crate::tools::{ApprovalRequest, Approver, Decision, Effect, Gate, ToolExecutor, WaveDecision};

/// Unattended uncertainty is a terminal, reviewable result, never an approval.
pub const PARKED: &str = "parked: needs the user's approval · the job will stop here";

/// The toolbox runs the normal Decider chain in auto mode, then sends any Ask here.
#[derive(Default)]
pub struct JobApprover {
    pending: Mutex<Option<String>>,
}

impl JobApprover {
    pub fn summary(&self) -> Option<String> {
        self.pending.lock().expect("job approval").clone()
    }

    pub fn park(&self, summary: String) {
        self.pending
            .lock()
            .expect("job approval")
            .get_or_insert(summary);
    }
}

impl Approver for JobApprover {
    fn request(&self, req: ApprovalRequest) -> Pin<Box<dyn Future<Output = Decision> + Send + '_>> {
        self.park(format!(
            "{PARKED}\n{} {}\n{}",
            req.tool, req.action, req.summary
        ));
        Box::pin(std::future::ready(Decision::Deny))
    }

    fn request_many(
        &self,
        reqs: Vec<ApprovalRequest>,
    ) -> Pin<Box<dyn Future<Output = WaveDecision> + Send + '_>> {
        for req in reqs {
            self.park(format!(
                "{PARKED}\n{} {}\n{}",
                req.tool, req.action, req.summary
            ));
        }
        Box::pin(std::future::ready(WaveDecision::DenyAll))
    }
}

/// Preserve the ordinary executor and policy, but stop all further execution on park.
pub struct JobExecutor {
    pub inner: Arc<dyn ToolExecutor>,
    pub approver: Arc<JobApprover>,
}

impl ToolExecutor for JobExecutor {
    fn defs(&self) -> Vec<ToolDef> {
        self.inner.defs()
    }

    fn defs_compact(&self) -> Vec<ToolDef> {
        self.inner.defs_compact()
    }

    fn is_read_only(&self, name: &str, args: &str) -> bool {
        self.inner.is_read_only(name, args)
    }

    fn effect(&self, name: &str, args: &str) -> Effect {
        self.inner.effect(name, args)
    }

    fn preapprove<'a>(
        &'a self,
        calls: &'a [(String, String)],
    ) -> Pin<Box<dyn Future<Output = Vec<Gate>> + Send + 'a>> {
        self.inner.preapprove(calls)
    }

    fn run<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
    ) -> Pin<Box<dyn Future<Output = (String, String)> + Send + 'a>> {
        self.run_gated(name, args, Gate::NotNeeded)
    }

    fn run_gated<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
        gate: Gate,
    ) -> Pin<Box<dyn Future<Output = (String, String)> + Send + 'a>> {
        Box::pin(async move {
            if self.approver.summary().is_some() {
                return (PARKED.into(), "parked".into());
            }
            if name == "batch" {
                let value: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
                let calls: Vec<_> = value["calls"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|call| {
                        Some((
                            call["tool"].as_str()?.to_string(),
                            call["arguments"].to_string(),
                        ))
                    })
                    .collect();
                if calls.is_empty()
                    || calls.len() > 8
                    || calls.iter().any(|(name, _)| name == "batch")
                {
                    return (
                        "batch needs 1–8 non-nested calls".into(),
                        "invalid arguments".into(),
                    );
                }
                let results = crate::tools::run_scheduled(self, &calls).await;
                if self.approver.summary().is_some() {
                    return (PARKED.into(), "parked".into());
                }
                let combined = results
                    .into_iter()
                    .map(|(result, _)| result)
                    .collect::<Vec<_>>()
                    .join("\n");
                return (
                    crate::tools::cap_head_tail(&combined, 32_000),
                    "batch".into(),
                );
            }
            // Non-workspace mutations can include script execution and skill installation:
            // they do not have the workspace approval/confinement path.
            if !self.inner.is_read_only(name, args)
                && !matches!(name, "fs" | "shell" | "proc" | "batch")
            {
                self.approver.park(format!("{PARKED}\n{name} {args}"));
                return (PARKED.into(), "parked".into());
            }
            let result = self.inner.run_gated(name, args, gate).await;
            if self.approver.summary().is_some() {
                (PARKED.into(), "parked".into())
            } else {
                result
            }
        })
    }

    fn supports_images(&self) -> bool {
        self.inner.supports_images()
    }

    fn space_files_dir(&self) -> Option<std::path::PathBuf> {
        self.inner.space_files_dir()
    }
}
