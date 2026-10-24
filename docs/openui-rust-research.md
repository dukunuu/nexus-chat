# OpenUI Lang in the native terminal renderer

## Primary sources

- [OpenUI language specification 1.0](https://github.com/thesysdev/openui/blob/main/spec/language.md)
- [Overview and worked streaming examples](https://github.com/thesysdev/openui/blob/main/spec/overview.md)
- [LibrarySpec and prompt contracts](https://github.com/thesysdev/openui/blob/main/spec/prompt.md)
- [Reference parser and incremental cache](https://github.com/thesysdev/openui/blob/main/packages/lang-core/src/parser/parser.ts)
- [Reference materializer](https://github.com/thesysdev/openui/blob/main/packages/lang-core/src/parser/materialize.ts)
- [Maintainers' account of the former Rust/WASM parser](https://github.com/thesysdev/openui/blob/main/docs/content/blog/rust-wasm-parser.mdx)

## What makes it different from JSON

OpenUI is a language, not line-delimited JSON. Programs bind names to expressions:
`root = Panel("Overview", [heading, metrics])`, followed by definitions such as
`heading = Hero("Monthly revenue")`. Component arguments are positional; the
host's component library defines their meaning. References are hoisted, so the
root can arrive first and individual children fill in later. Unresolved children
are omitted rather than reserving empty holes. [Language §§1–2; overview §§2.1–2.3.]

The reference implementation caches completed statement ASTs and reparses only
the pending tail. Its scanner respects strings, comments and bracket depth.
Pending strings/brackets are implicitly closed for display. An incomplete
redefinition must not replace a completed definition. Source replacement resets
the cache. [Language §4; reference parser `createStreamParser`.]

`$name` denotes local state; expressions derive values, binding props edit state,
and explicit actions such as `@Set` and `@ToAssistant` run on activation, never
just because the source was parsed. Queries/mutations are optional host features,
not a prerequisite for the component language. [Language §§5–6; prompt §4.2.]

The current upstream parser is TypeScript. The maintainers previously used Rust
compiled to WASM, then removed that boundary because copying/serializing small
results into JavaScript cost more than parsing. That browser-specific overhead
does not apply to a native Rust/ratatui application. Their incremental caching
approach remains relevant. [Maintainers' Rust/WASM article.]

## Nexus implementation boundary

Implement a bounded Rust dialect with the actual assignment/call/reference
syntax, root-first composition, positional component contracts, local state,
numeric expressions and explicit actions. Reuse the existing validated document
and transactional runtime internally; do not expose JSON as the authoring format.
Keep old JSON fences readable for existing transcripts and scripted apps.

This is a terminal component library, not a port of React components, and not a
claim of complete OpenUI 1.0 conformance. No Query/Mutation/network execution,
JavaScript coercion, arbitrary functions, browser navigation, dynamic loops or
host message protocol is implied. Unsupported syntax must fail explicitly.
Keep existing limits, theme roles, reviewed message submission, and hermetic tests.

Component argument order is a stored-data contract: append optional arguments,
never reorder existing ones. Prompts must describe only implemented features.
[Prompt §§1, 3.2, 4.]
