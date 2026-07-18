## Review

- **Medium — `src/app/swarm.rs:384,452,508-509`: duplicate persona names bypass the moderator gate.** `answered` is keyed by name, while roster editing permits duplicate names (`src/app/swarm.rs:109-143`). If one duplicate succeeds and another fails, both appear answered and moderation may return `Converged`. Track roster indices or stable persona IDs instead.
- **Medium — `src/app/swarm.rs:446-453`: empty responses count as successful answers.** Any `Ok(content)`, including blank content, enters `answered`. The provider can return `Ok("")` when response content is absent (`src/provider/openrouter.rs:483-488`). Require nonblank content before marking the persona answered.
- **Low — `src/app/swarm.rs:681-710`: tests cover only the name-based helper.** They do not exercise round sequencing, one opportunity per round, dynamic addition, failure retries, or the final cap. Add deterministic scheduler tests with a fake completion source.

No ownership/compiler issue was evident. Static inspection confirms roster snapshots give each row one opportunity per round, additions after rounds 1–3 participate in the next round, and timeouts plus `MAX_ROUNDS` bound termination.