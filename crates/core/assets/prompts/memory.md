Stored facts (numbered, may be empty):
{{facts}}

Latest exchange:
user: {{user}}
assistant: {{assistant}}

Reply with ONLY a JSON array of memory ops, no markdown, no prose. Each op is one of:
{"op":"add","text":"<durable single-line fact>"}
{"op":"update","id":<N>,"text":"<replacement>"}
{"op":"delete","id":<N>}
Empty array [] if nothing memory-worthy. Facts must be durable and user/project-relevant (preferences, identity, ongoing goals) — never a summary of what was just said. Merge/update instead of duplicating. Keep the total under 500 facts.
