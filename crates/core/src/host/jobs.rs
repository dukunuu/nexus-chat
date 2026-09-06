//! Bounded child-process execution outside the app actor.

use std::io;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt as _};

const OUTPUT_LIMIT: u64 = 1024 * 1024;

async fn read_output(reader: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err(io::Error::other("script output exceeded 1 MiB"));
    }
    Ok(bytes)
}

pub(super) async fn run(job: super::media::ScriptJob) -> Result<Value> {
    run_with_timeout(job, Duration::from_secs(30)).await
}

async fn run_with_timeout(job: super::media::ScriptJob, timeout: Duration) -> Result<Value> {
    let mut child = tokio::process::Command::new(&job.program)
        .args(&job.args)
        .current_dir(&job.cwd)
        .env("NEXUS_SPACE_ID", &job.space_id)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("stderr unavailable"))?;
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(child.wait(), read_output(stdout), read_output(stderr))
    })
    .await;
    match result {
        Ok(Ok((status, stdout, stderr))) => Ok(json!({
            "code": status.code(), "stdout": String::from_utf8_lossy(&stdout), "stderr": String::from_utf8_lossy(&stderr)
        })),
        error => {
            let _ = child.kill().await;
            match error {
                Ok(Err(error)) => Err(error.into()),
                _ => Err(anyhow!(
                    "script timed out after {} seconds",
                    timeout.as_secs()
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(text: &str) -> super::super::media::ScriptJob {
        super::super::media::ScriptJob {
            program: "sh".into(),
            args: vec!["-c".into(), text.into()],
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            space_id: "test".into(),
        }
    }

    #[tokio::test]
    async fn execution_is_bounded_and_reports_exit_status() {
        let result = run(shell("printf success; printf failure >&2; exit 7"))
            .await
            .unwrap();
        assert_eq!(result["code"], 7);
        assert_eq!(result["stdout"], "success");
        assert_eq!(result["stderr"], "failure");
        assert!(
            run_with_timeout(shell("while :; do :; done"), Duration::from_millis(50))
                .await
                .is_err()
        );
        assert!(
            run(shell("while :; do printf '0123456789abcdef'; done"))
                .await
                .is_err()
        );
    }
}
