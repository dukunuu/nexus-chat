//! Opt-in, local-only latency probe; silent in the normal gate.
use super::*;
use serde_json::json;
use std::time::{Duration, Instant};

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "manual dispatch/apply/atomic-write latency measurement"]
fn dispatch_apply_100_nodes_timing() -> Result<()> {
    let temp = Temp(std::env::temp_dir().join(format!("studio-timing-{}", Uuid::new_v4())));
    fs::create_dir_all(&temp.0)?;
    let mut backend = Backend::start(None, &temp.0.join("files"), &temp.0.join("data"))?;
    let mut value = serde_json::to_value(&backend.deck)?;
    value["slides"] = json!([{"id":"s1", "children":(0..100).map(|i| json!({
        "id":format!("n{i}"), "kind":"shape", "shape":"rect", "fill":"accent",
        "position":{"x":i % 10 * 180, "y":i / 10 * 90}, "sizing":{"w":100,"h":60}
    })).collect::<Vec<_>>()}]);
    backend.deck = Deck::check(&value.to_string()).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    backend.save()?;
    backend.known_revision = backend.deck.revision();
    backend.deck_stamp = stamp(&backend.path);
    let mut apply = Vec::new();
    let mut write = Vec::new();
    let mut dispatch = Vec::new();
    for x in 1..=40 {
        let ops = json!([{"op":"update_node", "id":"n0", "base_hash":backend.deck.node("n0").context("node")?.hash(), "fields":{"position":{"x":x,"y":0}}}]);
        let parsed: Vec<Op> = serde_json::from_value(ops.clone())?;
        let start = Instant::now();
        let next = deck::apply(&backend.deck, &parsed)?;
        apply.push(start.elapsed());
        let bytes = serde_json::to_vec_pretty(&next)?;
        let start = Instant::now();
        atomic_write(&temp.0.join("probe.deck.json"), &bytes)?;
        write.push(start.elapsed());
        let command = json!({"command":"Apply", "ops":ops}).to_string();
        let start = Instant::now();
        let event = backend.dispatch(&command);
        dispatch.push(start.elapsed());
        assert!(matches!(event, Event::Deck { .. }), "{event:?}");
    }
    let report = |label: &str, mut samples: Vec<Duration>| {
        samples.sort_unstable();
        eprintln!(
            "{label}: median {:.3} ms, p95 {:.3} ms (40 samples)",
            samples[20].as_secs_f64() * 1000.0,
            samples[38].as_secs_f64() * 1000.0
        );
    };
    report("apply/check 100 nodes", apply);
    report("atomic write (temp+sync_all+rename)", write);
    report(
        "dispatch(Apply), including event + feedback persistence",
        dispatch,
    );
    Ok(())
}
