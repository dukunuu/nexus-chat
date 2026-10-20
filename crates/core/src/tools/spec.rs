//! Shared definitions, validation, routing and read-only classes for table tools.

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::provider::ToolDef;

pub(super) enum Kind {
    Str,
    // Script writes require a string, but deliberately allow empty content.
    StrOrEmpty,
    Int,
    Num,
    Bool,
    StrList,
    Edits,
    Events,
    Enum(&'static [&'static str]),
}

pub(super) struct Param {
    name: &'static str,
    kind: Kind,
    doc: &'static str,
}

pub(super) struct ActionSpec {
    name: &'static str,
    summary: &'static str,
    required: &'static [&'static str],
    optional: &'static [&'static str],
    read_only: bool,
    internal: &'static str,
}

pub(super) struct ToolSpec {
    name: &'static str,
    purpose: &'static str,
    selector: Option<&'static str>,
    params: &'static [Param],
    actions: &'static [ActionSpec],
}

impl Param {
    fn schema(&self) -> Value {
        let mut schema = match self.kind {
            Kind::Str | Kind::StrOrEmpty => json!({ "type": "string" }),
            Kind::Int => json!({ "type": "integer" }),
            Kind::Num => json!({ "type": "number" }),
            Kind::Bool => json!({ "type": "boolean" }),
            Kind::StrList => json!({ "type": "array", "items": { "type": "string" } }),
            Kind::Edits => json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "hash": { "type": "string", "description": "hash from the latest read" },
                        "line": { "type": "integer", "description": "optional 1-based line number to disambiguate repeated lines" },
                        "new": { "type": ["string", "null"] }
                    },
                    "required": ["hash"]
                }
            }),
            Kind::Events => json!({
                "type": "array", "maxItems": 100,
                "items": { "type": "object", "properties": {
                    "type": { "type": "string", "enum": ["button", "key", "input", "tick"] },
                    "action": { "type": "string" }, "key": { "type": "string" },
                    "value": { "type": ["string", "boolean", "number"] }
                }, "required": ["type"] }
            }),
            Kind::Enum(values) => json!({ "type": "string", "enum": values }),
        };
        schema["description"] = json!(self.doc);
        schema
    }

    fn is_present(&self, value: &Value) -> bool {
        let Some(value) = value.get(self.name) else {
            return false;
        };
        match self.kind {
            Kind::Str | Kind::Enum(_) => value.as_str().is_some_and(|s| !s.trim().is_empty()),
            Kind::StrOrEmpty => value.is_string(),
            // Preserve files.pdf_page's as_u64 check, including acceptance of zero.
            Kind::Int => value.as_u64().is_some(),
            Kind::Num => value.is_number(),
            Kind::Bool => value.is_boolean(),
            Kind::StrList | Kind::Edits => value.as_array().is_some_and(|a| !a.is_empty()),
            Kind::Events => value.is_array(),
        }
    }
}

impl ToolSpec {
    pub(super) fn def(&self, live: &[&str]) -> ToolDef {
        let actions: Vec<_> = self
            .actions
            .iter()
            .filter(|action| live.contains(&action.name))
            .collect();
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        if let Some(selector) = self.selector {
            let names: Vec<_> = actions.iter().map(|action| action.name).collect();
            properties.insert(
                selector.to_string(),
                json!({ "type": "string", "enum": names }),
            );
            required.push(selector);
        }
        for param in self.params {
            if actions.iter().any(|action| {
                action.required.contains(&param.name) || action.optional.contains(&param.name)
            }) {
                properties.insert(param.name.to_string(), param.schema());
            }
            if (self.selector.is_none() || self.name == "search")
                && !actions.is_empty()
                && actions
                    .iter()
                    .all(|action| action.required.contains(&param.name))
            {
                required.push(param.name);
            }
        }
        if self.name == "task" {
            properties.get_mut("tools").expect("task tools")["default"] = json!("read-only");
            let steps = properties.get_mut("max_steps").expect("task steps");
            steps["default"] = json!(20);
            steps["minimum"] = json!(1);
            steps["maximum"] = json!(40);
        }
        let mut description = self.purpose.to_string();
        for action in actions {
            description.push_str("\n- ");
            if let Some(selector) = self.selector {
                let _ = write!(description, "{selector}=");
            }
            let _ = write!(description, "{}: {}.", action.name, action.summary);
            if !action.required.is_empty() {
                let _ = write!(description, " Needs {}", action.required.join(", "));
            }
            if !action.optional.is_empty() {
                description.push_str(if action.required.is_empty() {
                    " "
                } else {
                    "; "
                });
                let _ = write!(description, "optional {}", action.optional.join(", "));
            }
            if !action.required.is_empty() || !action.optional.is_empty() {
                description.push('.');
            }
        }
        super::tool_def(
            self.name,
            &description,
            json!({ "type": "object", "properties": properties, "required": required }),
        )
    }

    pub(super) fn def_compact(&self, live: &[&str]) -> ToolDef {
        let mut def = self.def(live);
        let mut description = self.purpose.to_string();
        for action in self
            .actions
            .iter()
            .filter(|action| live.contains(&action.name))
        {
            description.push_str("\n- ");
            if let Some(selector) = self.selector {
                let _ = write!(description, "{selector}=");
            }
            description.push_str(action.name);
            if !action.required.is_empty() {
                let _ = write!(description, " (needs {})", action.required.join(", "));
            }
        }
        def.description = description;
        super::strip_param_descriptions(&mut def.parameters);
        def
    }

    fn selected_action(&self, value: &Value) -> Option<&ActionSpec> {
        match self.selector {
            Some(selector) => {
                let name = value.get(selector)?.as_str()?;
                self.actions.iter().find(|action| action.name == name)
            }
            None => self.actions.first(),
        }
    }

    pub(super) fn validate(&self, value: &Value) -> Result<&'static str, String> {
        let valid = || {
            self.actions
                .iter()
                .map(|action| action.name)
                .collect::<Vec<_>>()
                .join(", ")
        };
        if let Some(selector) = self.selector {
            let given = value
                .get(selector)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    format!(
                        "missing required field: {selector} · {} needs one of: {}",
                        self.name,
                        valid()
                    )
                })?;
            if self.selected_action(value).is_none() {
                return Err(format!(
                    "invalid {selector} for {}: {given:?} · valid: {}",
                    self.name,
                    valid()
                ));
            }
        }
        if self.name == "task" {
            if value
                .get("tools")
                .is_some_and(|tools| !matches!(tools.as_str(), Some("read-only" | "workspace")))
            {
                return Err("invalid tool arguments: tools must be read-only or workspace".into());
            }
            if value.get("max_steps").is_some_and(|steps| {
                !steps
                    .as_u64()
                    .is_some_and(|steps| (1..=40).contains(&steps))
            }) {
                return Err(
                    "invalid tool arguments: max_steps must be an integer from 1 to 40".into(),
                );
            }
        }
        let action = self.selected_action(value).expect("validated action");
        for name in action.required {
            let param = self
                .params
                .iter()
                .find(|param| param.name == *name)
                .expect("declared param");
            if !param.is_present(value) {
                return Err(format!(
                    "missing required field: {name} · {}.{} needs it ({})",
                    self.name, action.name, param.doc
                ));
            }
        }
        Ok(action.internal)
    }
}

pub(super) fn find(name: &str) -> Option<&'static ToolSpec> {
    all_tools().find(|tool| tool.name == name)
}

pub(super) fn current_tools() -> impl Iterator<Item = (&'static str, Vec<&'static str>)> {
    all_tools().map(|tool| {
        (
            tool.name,
            tool.actions.iter().map(|action| action.name).collect(),
        )
    })
}

pub(super) fn public_alias(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    all_tools().find_map(|tool| {
        let action = tool.actions.iter().find(|action| action.internal == name)?;
        Some((tool.name, tool.selector?, action.name))
    })
}

pub(super) fn read_only(name: &str, args: &str) -> Option<bool> {
    if name == "task" {
        let value = serde_json::from_str::<Value>(args).unwrap_or_default();
        return Some(value.get("tools").is_none_or(|tools| tools == "read-only"));
    }
    if let Some(tool) = find(name) {
        let value = serde_json::from_str::<Value>(args).unwrap_or_default();
        return Some(
            tool.selected_action(&value)
                .is_some_and(|action| action.read_only),
        );
    }
    all_tools()
        .flat_map(|tool| tool.actions)
        .find(|action| action.internal == name)
        .map(|action| action.read_only)
}

pub(super) static SKILLS: ToolSpec = ToolSpec {
    name: "skills",
    purpose: "Manage Agent Skills from app, project, and user/global roots; create and install write only to the app-managed root.",
    selector: Some("action"),
    params: &[
        Param {
            name: "name",
            kind: Kind::Str,
            doc: "skill name for load/create",
        },
        Param {
            name: "file",
            kind: Kind::Str,
            doc: "optional path within the skill for load; defaults to SKILL.md",
        },
        Param {
            name: "description",
            kind: Kind::Str,
            doc: "short description for create",
        },
        Param {
            name: "body",
            kind: Kind::Str,
            doc: "skill instructions for create",
        },
        Param {
            name: "overwrite",
            kind: Kind::Bool,
            doc: "replace an existing skill (default false)",
        },
        Param {
            name: "source",
            kind: Kind::Str,
            doc: "GitHub owner/repo/path for install",
        },
    ],
    actions: &[
        ActionSpec {
            name: "load",
            summary: "Read full skill instructions or a relative file",
            required: &["name"],
            optional: &["file"],
            read_only: true,
            internal: "skill",
        },
        ActionSpec {
            name: "create",
            summary: "Create a skill",
            required: &["name", "description"],
            optional: &["body", "overwrite"],
            read_only: false,
            internal: "create_skill",
        },
        ActionSpec {
            name: "install",
            summary: "Install a skill from GitHub",
            required: &["source"],
            optional: &[],
            read_only: false,
            internal: "install_skill",
        },
    ],
};

pub(super) static TUI_APP: ToolSpec = ToolSpec {
    name: "tui_app",
    purpose: "Sandboxed Rhai terminal apps in tui_apps/<name>/app.rhai. Export init()->map, update(state,event)->map, view(state)->#{title,view:[nexus-ui nodes or canvas]}. Canvas: #{type:canvas,rows:1-16,cells:[[x,y,char,tone]]}. state.tick_ms>=100 enables ticks. No host access. Preview before open; read gives hash-lines. Names: [a-z0-9-]{1,32}.",
    selector: Some("action"),
    params: &[
        Param {
            name: "name",
            kind: Kind::Str,
            doc: "space app name",
        },
        Param {
            name: "content",
            kind: Kind::StrOrEmpty,
            doc: "complete Rhai script",
        },
        Param {
            name: "edits",
            kind: Kind::Edits,
            doc: "hash-line edits from read",
        },
        Param {
            name: "events",
            kind: Kind::Events,
            doc: "ordered button/action, key/key, input/key/value or tick events",
        },
        Param {
            name: "width",
            kind: Kind::Int,
            doc: "preview columns, 1–160 (default 60)",
        },
        Param {
            name: "height",
            kind: Kind::Int,
            doc: "preview rows, 1–60 (default 20)",
        },
    ],
    actions: &[
        ActionSpec {
            name: "list",
            summary: "List apps",
            required: &[],
            optional: &[],
            read_only: true,
            internal: "tui_app_list",
        },
        ActionSpec {
            name: "read",
            summary: "Read script",
            required: &["name"],
            optional: &[],
            read_only: true,
            internal: "tui_app_read",
        },
        ActionSpec {
            name: "write",
            summary: "Write script",
            required: &["name", "content"],
            optional: &[],
            read_only: false,
            internal: "tui_app_write",
        },
        ActionSpec {
            name: "edit",
            summary: "Edit script",
            required: &["name", "edits"],
            optional: &[],
            read_only: false,
            internal: "tui_app_edit",
        },
        ActionSpec {
            name: "preview",
            summary: "Render grid, log and errors",
            required: &["name"],
            optional: &["events", "width", "height"],
            read_only: true,
            internal: "tui_app_preview",
        },
        ActionSpec {
            name: "open",
            summary: "Open popup",
            required: &["name"],
            optional: &[],
            read_only: false,
            internal: "tui_app_open",
        },
    ],
};

pub(super) static SCRIPTS: ToolSpec = ToolSpec {
    name: "scripts",
    purpose: "Manage confined space scripts, run skill or space scripts, and install packages.",
    selector: Some("action"),
    params: &[
        Param {
            name: "path",
            kind: Kind::Str,
            doc: "script path relative to the scripts dir",
        },
        Param {
            name: "content",
            kind: Kind::StrOrEmpty,
            doc: "complete script content for write",
        },
        Param {
            name: "offset",
            kind: Kind::Int,
            doc: "1-based starting line (default 1)",
        },
        Param {
            name: "limit",
            kind: Kind::Int,
            doc: "maximum 200 lines",
        },
        Param {
            name: "edits",
            kind: Kind::Edits,
            doc: "hash-line edits from the latest read",
        },
        Param {
            name: "code",
            kind: Kind::Str,
            doc: "Python source for python",
        },
        Param {
            name: "name",
            kind: Kind::Str,
            doc: "confined .py filename for python",
        },
        Param {
            name: "temporary",
            kind: Kind::Bool,
            doc: "delete the script after python runs (default false)",
        },
        Param {
            name: "skill",
            kind: Kind::Str,
            doc: "skill name for run (unless space=true), or pip target for install",
        },
        Param {
            name: "space",
            kind: Kind::Bool,
            doc: "run from the space scripts directory",
        },
        Param {
            name: "args",
            kind: Kind::StrList,
            doc: "command-line arguments for run/python",
        },
        Param {
            name: "packages",
            kind: Kind::StrList,
            doc: "packages for install",
        },
        Param {
            name: "app",
            kind: Kind::Str,
            doc: "app npm target for install",
        },
    ],
    actions: &[
        ActionSpec {
            name: "list",
            summary: "List space script files",
            required: &[],
            optional: &[],
            read_only: true,
            internal: "list_scripts",
        },
        ActionSpec {
            name: "read",
            summary: "Read a script with stable hash-lines",
            required: &["path"],
            optional: &["offset", "limit"],
            read_only: true,
            internal: "read_script",
        },
        ActionSpec {
            name: "write",
            summary: "Replace a script's complete content",
            required: &["path", "content"],
            optional: &[],
            read_only: false,
            internal: "write_script",
        },
        ActionSpec {
            name: "edit",
            summary: "Apply hash-line edits to a script",
            required: &["path", "edits"],
            optional: &[],
            read_only: false,
            internal: "edit_script",
        },
        ActionSpec {
            name: "run",
            summary: "Run an existing skill or space script",
            required: &["path"],
            optional: &["skill", "space", "args"],
            read_only: false,
            internal: "run_script",
        },
        ActionSpec {
            name: "python",
            summary: "Write and run Python in the space environment, persisting unless temporary=true",
            required: &["code", "name"],
            optional: &["temporary", "args"],
            read_only: false,
            internal: "run_python",
        },
        ActionSpec {
            name: "install",
            summary: "Install packages into at most one skill or app target, defaulting to the space Python environment",
            required: &["packages"],
            optional: &["skill", "app"],
            read_only: false,
            internal: "install_packages",
        },
    ],
};

pub(super) static SEARCH: ToolSpec = ToolSpec {
    name: "search",
    purpose: "Search the web, scholarly literature, or HN/Reddit discussions.",
    selector: Some("mode"),
    params: &[
        Param {
            name: "query",
            kind: Kind::Str,
            doc: "search query",
        },
        Param {
            name: "recency",
            kind: Kind::Enum(&["day", "week", "month", "year"]),
            doc: "web result recency filter",
        },
        Param {
            name: "include_domains",
            kind: Kind::StrList,
            doc: "restrict web results to these domains",
        },
        Param {
            name: "exclude_domains",
            kind: Kind::StrList,
            doc: "exclude these domains from web results",
        },
        Param {
            name: "limit",
            kind: Kind::Int,
            doc: "maximum academic results (default 10, max 20)",
        },
    ],
    actions: &[
        ActionSpec {
            name: "web",
            summary: "Search using the configured web backend",
            required: &["query"],
            optional: &["recency", "include_domains", "exclude_domains"],
            read_only: true,
            internal: "web_search",
        },
        ActionSpec {
            name: "academic",
            summary: "Search Semantic Scholar, preserving scholarly metadata",
            required: &["query"],
            optional: &["limit"],
            read_only: true,
            internal: "academic_search",
        },
        ActionSpec {
            name: "discussion",
            summary: "Search HN and Reddit, preserving engagement metadata",
            required: &["query"],
            optional: &[],
            read_only: true,
            internal: "discussion_search",
        },
    ],
};

pub(super) static FETCH_URL: ToolSpec = ToolSpec {
    name: "fetch_url",
    purpose: "Fetch readable, paged text from URLs, PDFs, or YouTube transcripts, using the 24-hour cache and respecting verifier cache-only mode.",
    selector: None,
    params: &[
        Param {
            name: "url",
            kind: Kind::Str,
            doc: "URL to fetch",
        },
        Param {
            name: "offset",
            kind: Kind::Int,
            doc: "1-based starting line (default 1)",
        },
        Param {
            name: "limit",
            kind: Kind::Int,
            doc: "maximum 200 lines",
        },
        Param {
            name: "fresh",
            kind: Kind::Bool,
            doc: "bypass the cache (default false)",
        },
    ],
    actions: &[ActionSpec {
        name: "fetch_url",
        summary: "Fetch a page of text",
        required: &["url"],
        optional: &["offset", "limit", "fresh"],
        read_only: true,
        internal: "fetch_url",
    }],
};

pub(super) static FS: ToolSpec = ToolSpec {
    name: "fs",
    purpose: "Explore, read and safely change files confined to the agent workspace, respecting gitignore rules. Read existing files before changing them; stale changes are refused.",
    selector: Some("action"),
    params: &[
        Param {
            name: "path",
            kind: Kind::Str,
            doc: "workspace path (default . for list, glob and grep)",
        },
        Param {
            name: "depth",
            kind: Kind::Int,
            doc: "list depth (default 2, maximum 6)",
        },
        Param {
            name: "pattern",
            kind: Kind::Str,
            doc: "relative glob for glob, literal case-sensitive substring for grep",
        },
        Param {
            name: "offset",
            kind: Kind::Int,
            doc: "1-based starting line (default 1)",
        },
        Param {
            name: "limit",
            kind: Kind::Int,
            doc: "read line count (default 400, maximum 2000)",
        },
        Param {
            name: "context",
            kind: Kind::Int,
            doc: "grep context lines (default 0, maximum 5)",
        },
        Param {
            name: "edits",
            kind: Kind::Edits,
            doc: "hash-line edits from the latest read; null or omitted new deletes the line",
        },
        Param {
            name: "content",
            kind: Kind::StrOrEmpty,
            doc: "complete file content (may be empty)",
        },
        Param {
            name: "to",
            kind: Kind::Str,
            doc: "new workspace path; must not exist",
        },
    ],
    actions: &[
        ActionSpec {
            name: "list",
            summary: "List a gitignore-aware tree, capped at 400 entries",
            required: &[],
            optional: &["path", "depth"],
            read_only: true,
            internal: "fs_list",
        },
        ActionSpec {
            name: "glob",
            summary: "Find sorted relative paths, capped at 400 entries",
            required: &["pattern"],
            optional: &["path"],
            read_only: true,
            internal: "fs_glob",
        },
        ActionSpec {
            name: "grep",
            summary: "Search non-binary files, grouped by file and capped at 200 matches",
            required: &["pattern"],
            optional: &["path", "context"],
            read_only: true,
            internal: "fs_grep",
        },
        ActionSpec {
            name: "read",
            summary: "Read paged hash-lines and record a file stamp",
            required: &["path"],
            optional: &["offset", "limit"],
            read_only: true,
            internal: "fs_read",
        },
        ActionSpec {
            name: "edit",
            summary: "Apply hash-line edits after a fresh read, returning a unified diff",
            required: &["path", "edits"],
            optional: &[],
            read_only: false,
            internal: "fs_edit",
        },
        ActionSpec {
            name: "write",
            summary: "Atomically create or replace a file, returning a unified diff",
            required: &["path", "content"],
            optional: &[],
            read_only: false,
            internal: "fs_write",
        },
        ActionSpec {
            name: "move",
            summary: "Move a freshly read file to a new workspace path",
            required: &["path", "to"],
            optional: &[],
            read_only: false,
            internal: "fs_move",
        },
        ActionSpec {
            name: "delete",
            summary: "Delete a freshly read file, returning its removed line count and diff",
            required: &["path"],
            optional: &[],
            read_only: false,
            internal: "fs_delete",
        },
    ],
};

pub(super) static SHELL: ToolSpec = ToolSpec {
    name: "shell",
    purpose: "Run one-shot commands in the agent workspace without privilege escalation, or page their saved output logs.",
    selector: Some("action"),
    params: &[
        Param {
            name: "cmd",
            kind: Kind::Str,
            doc: "shell command to run with sh -c",
        },
        Param {
            name: "cwd",
            kind: Kind::Str,
            doc: "workspace directory (default workspace root)",
        },
        Param {
            name: "timeout_s",
            kind: Kind::Int,
            doc: "timeout in seconds (default 120, maximum 600)",
        },
        Param {
            name: "id",
            kind: Kind::Str,
            doc: "saved log id returned by run",
        },
        Param {
            name: "offset",
            kind: Kind::Int,
            doc: "1-based starting line (default 1, 400 lines per page)",
        },
    ],
    actions: &[
        ActionSpec {
            name: "run",
            summary: "Run a noninteractive command, returning its exit code and head/tail output",
            required: &["cmd"],
            optional: &["cwd", "timeout_s"],
            read_only: false,
            internal: "shell_run",
        },
        ActionSpec {
            name: "log",
            summary: "Page the full saved output of a command",
            required: &["id"],
            optional: &["offset"],
            read_only: true,
            internal: "shell_log",
        },
    ],
};

pub(super) static PROC: ToolSpec = ToolSpec {
    name: "proc",
    purpose: "Start and manage session-owned background commands in the agent workspace. Commands stop when Nexus exits.",
    selector: Some("action"),
    params: &[
        Param {
            name: "cmd",
            kind: Kind::Str,
            doc: "background command to run with sh -c",
        },
        Param {
            name: "name",
            kind: Kind::Str,
            doc: "short display name (default command)",
        },
        Param {
            name: "cwd",
            kind: Kind::Str,
            doc: "workspace directory (default workspace root)",
        },
        Param {
            name: "ready_when",
            kind: Kind::Str,
            doc: "output substring indicating readiness (literal, not a regex)",
        },
        Param {
            name: "ready_port",
            kind: Kind::Int,
            doc: "localhost TCP port indicating readiness",
        },
        Param {
            name: "wait_s",
            kind: Kind::Int,
            doc: "readiness grace period in seconds (default 5, maximum 60)",
        },
        Param {
            name: "id",
            kind: Kind::Str,
            doc: "8-character process id returned by start or list",
        },
        Param {
            name: "since",
            kind: Kind::Int,
            doc: "1-based starting line; default new output since the previous read",
        },
    ],
    actions: &[
        ActionSpec {
            name: "start",
            summary: "Start a server, watcher or long command, returning when ready, exited or after the grace period",
            required: &["cmd"],
            optional: &["name", "cwd", "ready_when", "ready_port", "wait_s"],
            read_only: false,
            internal: "proc_start",
        },
        ActionSpec {
            name: "read",
            summary: "Read new output with the running or exited state and head/tail shaping",
            required: &["id"],
            optional: &["since"],
            read_only: true,
            internal: "proc_read",
        },
        ActionSpec {
            name: "list",
            summary: "List processes, this session first, with state, uptime and last output line",
            required: &[],
            optional: &[],
            read_only: true,
            internal: "proc_list",
        },
        ActionSpec {
            name: "kill",
            summary: "Stop a process group without asking for approval",
            required: &["id"],
            optional: &[],
            read_only: false,
            internal: "proc_kill",
        },
    ],
};

pub(super) static RESEARCH_LOOKUP: ToolSpec = ToolSpec {
    name: "research_lookup",
    purpose: "Look up previously gathered research material.",
    selector: Some("scope"),
    params: &[Param {
        name: "query",
        kind: Kind::Str,
        doc: "keywords; optional for citations, required for session_sources",
    }],
    actions: &[
        ActionSpec {
            name: "session_sources",
            summary: "Search this research session's source bundle",
            required: &["query"],
            optional: &[],
            read_only: true,
            internal: "search_sources",
        },
        ActionSpec {
            name: "citations",
            summary: "Search citations saved across this space",
            required: &[],
            optional: &["query"],
            read_only: true,
            internal: "list_citations",
        },
    ],
};

pub(super) static FILES: ToolSpec = ToolSpec {
    name: "files",
    purpose: "Work with imported space files.",
    selector: Some("action"),
    params: &[
        Param {
            name: "query",
            kind: Kind::Str,
            doc: "search query",
        },
        Param {
            name: "name",
            kind: Kind::Str,
            doc: "imported file name",
        },
        Param {
            name: "offset",
            kind: Kind::Int,
            doc: "1-based starting line (default 1)",
        },
        Param {
            name: "limit",
            kind: Kind::Int,
            doc: "maximum 200 lines",
        },
        Param {
            name: "page",
            kind: Kind::Int,
            doc: "1-based PDF page",
        },
    ],
    actions: &[
        ActionSpec {
            name: "search",
            summary: "Search imported files by semantic similarity or keywords",
            required: &["query"],
            optional: &[],
            read_only: true,
            internal: "search_files",
        },
        ActionSpec {
            name: "read",
            summary: "Read a page of extracted text",
            required: &["name"],
            optional: &["offset", "limit"],
            read_only: true,
            internal: "read_file",
        },
        ActionSpec {
            name: "pdf_page",
            summary: "Return an imported PDF page image when available",
            required: &["name", "page"],
            optional: &[],
            read_only: true,
            internal: "read_pdf_page",
        },
    ],
};

pub(super) static MEDIA: ToolSpec = ToolSpec {
    name: "media",
    purpose: "Generate and transform media, and manage named video image references.",
    selector: Some("action"),
    params: &[
        Param {
            name: "prompt",
            kind: Kind::Str,
            doc: "generation prompt",
        },
        Param {
            name: "image_id",
            kind: Kind::Str,
            doc: "reference image for generate_image, or image being saved for save_reference",
        },
        Param {
            name: "size",
            kind: Kind::Str,
            doc: "image size (default 1024x1024)",
        },
        Param {
            name: "duration",
            kind: Kind::Int,
            doc: "video duration in seconds (default 6, normalized per model)",
        },
        Param {
            name: "resolution",
            kind: Kind::Str,
            doc: "video resolution (default 720p, normalized per model)",
        },
        Param {
            name: "aspect_ratio",
            kind: Kind::Str,
            doc: "video aspect ratio (default 16:9, normalized per model)",
        },
        Param {
            name: "generate_audio",
            kind: Kind::Bool,
            doc: "generate video audio (default false)",
        },
        Param {
            name: "first_frame_id",
            kind: Kind::Str,
            doc: "image ID for the first video frame",
        },
        Param {
            name: "last_frame_id",
            kind: Kind::Str,
            doc: "image ID for the last video frame",
        },
        Param {
            name: "ref_image_id",
            kind: Kind::Str,
            doc: "video reference image ID",
        },
        Param {
            name: "character_refs",
            kind: Kind::StrList,
            doc: "saved character reference names",
        },
        Param {
            name: "location_refs",
            kind: Kind::StrList,
            doc: "saved location reference names",
        },
        Param {
            name: "seed",
            kind: Kind::Int,
            doc: "video generation seed",
        },
        Param {
            name: "source_video_id",
            kind: Kind::Str,
            doc: "source video ID for provider video continuation",
        },
        Param {
            name: "video_id",
            kind: Kind::Str,
            doc: "local video ID without .mp4",
        },
        Param {
            name: "video_ids",
            kind: Kind::StrList,
            doc: "ordered video IDs to concatenate",
        },
        Param {
            name: "lighting",
            kind: Kind::Enum(&["noir", "warm", "cold", "vintage", "vivid", "bleach_bypass"]),
            doc: "lighting preset for edit",
        },
        Param {
            name: "camera_move",
            kind: Kind::Enum(&[
                "dolly_in",
                "dolly_out",
                "pan_left",
                "pan_right",
                "tilt_up",
                "tilt_down",
            ]),
            doc: "camera movement for edit",
        },
        Param {
            name: "intensity",
            kind: Kind::Num,
            doc: "effect intensity, clamped to 0–1 (default 0.5)",
        },
        Param {
            name: "speed",
            kind: Kind::Num,
            doc: "positive playback speed factor",
        },
        Param {
            name: "trim_start",
            kind: Kind::Num,
            doc: "non-negative trim start in seconds",
        },
        Param {
            name: "trim_end",
            kind: Kind::Num,
            doc: "non-negative trim end in seconds",
        },
        Param {
            name: "remove_audio",
            kind: Kind::Bool,
            doc: "remove video audio (default false)",
        },
        Param {
            name: "time_sec",
            kind: Kind::Num,
            doc: "frame timestamp in seconds (default 0)",
        },
        Param {
            name: "format",
            kind: Kind::Enum(&["png", "jpg"]),
            doc: "extracted image format (default png)",
        },
        Param {
            name: "name",
            kind: Kind::Str,
            doc: "reference name for save_reference/delete_reference",
        },
        Param {
            name: "description",
            kind: Kind::Str,
            doc: "reference description for save_reference",
        },
        Param {
            name: "type",
            kind: Kind::Str,
            doc: "reference type (default character)",
        },
    ],
    actions: &[
        ActionSpec {
            name: "generate_image",
            summary: "Generate an image, optionally using a pasted image reference",
            required: &["prompt"],
            optional: &["image_id", "size"],
            read_only: false,
            internal: "generate_image",
        },
        ActionSpec {
            name: "generate_video",
            summary: "Generate a video from text and optional frame or reference images",
            required: &["prompt"],
            optional: &[
                "duration",
                "resolution",
                "aspect_ratio",
                "generate_audio",
                "first_frame_id",
                "last_frame_id",
                "ref_image_id",
                "character_refs",
                "location_refs",
                "seed",
                "source_video_id",
            ],
            read_only: false,
            internal: "generate_video",
        },
        ActionSpec {
            name: "edit",
            summary: "Transform a video locally with ffmpeg",
            required: &["video_id"],
            optional: &[
                "lighting",
                "camera_move",
                "intensity",
                "speed",
                "trim_start",
                "trim_end",
                "remove_audio",
            ],
            read_only: false,
            internal: "edit_video",
        },
        ActionSpec {
            name: "extract_frame",
            summary: "Extract a video frame locally with ffmpeg",
            required: &["video_id"],
            optional: &["time_sec", "format"],
            read_only: false,
            internal: "extract_frame",
        },
        ActionSpec {
            name: "stitch",
            summary: "Concatenate video clips locally with ffmpeg",
            required: &["video_ids"],
            optional: &[],
            read_only: false,
            internal: "stitch_videos",
        },
        ActionSpec {
            name: "save_reference",
            summary: "Save a named image reference for video consistency",
            required: &["name", "image_id", "description"],
            optional: &["type"],
            read_only: false,
            internal: "save_reference",
        },
        ActionSpec {
            name: "list_references",
            summary: "List saved image references",
            required: &[],
            optional: &[],
            read_only: true,
            internal: "list_references",
        },
        ActionSpec {
            name: "delete_reference",
            summary: "Delete a named image reference",
            required: &["name"],
            optional: &[],
            read_only: false,
            internal: "delete_reference",
        },
    ],
};

pub(super) static PROPOSE_COMMAND: ToolSpec = ToolSpec {
    name: "propose_command",
    purpose: "Hand the user a command to run themselves in their own terminal: anything needing root, a password, or their interactive terminal. It is placed in their composer as !<cmd> and runs only when they press Enter; its output then appears in the conversation.",
    selector: None,
    params: &[
        Param {
            name: "cmd",
            kind: Kind::Str,
            doc: "exact shell command for the user to run",
        },
        Param {
            name: "reason",
            kind: Kind::Str,
            doc: "one line on why it is needed, shown to the user",
        },
    ],
    actions: &[ActionSpec {
        name: "propose_command",
        summary: "Propose a command for the user to run",
        required: &["cmd", "reason"],
        optional: &[],
        read_only: true,
        internal: "propose_command",
    }],
};

pub(super) static REQUEST_WORKSPACE: ToolSpec = ToolSpec {
    name: "request_workspace",
    purpose: "Ask the user to switch this session into agent mode on a directory, which gives you the fs and shell workspace tools from your next turn. The user approves or denies.",
    selector: None,
    params: &[
        Param {
            name: "root",
            kind: Kind::Str,
            doc: "absolute path of the directory to work in",
        },
        Param {
            name: "reason",
            kind: Kind::Str,
            doc: "one line on why you need it, shown to the user",
        },
    ],
    actions: &[ActionSpec {
        name: "request_workspace",
        summary: "Request agent mode in a directory",
        required: &["root", "reason"],
        optional: &[],
        read_only: false,
        internal: "request_workspace",
    }],
};

pub(super) static TASK: ToolSpec = ToolSpec {
    name: "task",
    purpose: "Delegate a focused investigation to a fresh sub-agent; only its report returns. Workspace tasks require approval and return a branch and diff, never edits to the main tree.",
    selector: None,
    params: &[
        Param {
            name: "prompt",
            kind: Kind::Str,
            doc: "self-contained task for the helper",
        },
        Param {
            name: "tools",
            kind: Kind::Enum(&["read-only", "workspace"]),
            doc: "capabilities (default read-only)",
        },
        Param {
            name: "max_steps",
            kind: Kind::Int,
            doc: "maximum tool rounds, 1–40 (default 20)",
        },
    ],
    actions: &[ActionSpec {
        name: "task",
        summary: "Run a focused helper with the session model",
        required: &["prompt"],
        optional: &["tools", "max_steps"],
        read_only: true,
        internal: "task",
    }],
};

const TOOLS: &[&ToolSpec] = &[
    &TASK,
    &SKILLS,
    &SCRIPTS,
    &TUI_APP,
    &SEARCH,
    &FETCH_URL,
    &FS,
    &SHELL,
    &PROC,
    &RESEARCH_LOOKUP,
    &FILES,
    &MEDIA,
];

/// Tools that hand a step to the user instead of acting. The frontend (a
/// terminal UI, an approver) decides whether they are offered; they route
/// through the same validation and read-only classes as `TOOLS`.
const USER_BRIDGES: &[&ToolSpec] = &[&PROPOSE_COMMAND, &REQUEST_WORKSPACE];

fn all_tools() -> impl Iterator<Item = &'static ToolSpec> {
    TOOLS.iter().chain(USER_BRIDGES).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolBox, is_read_only_tool, public_call};

    #[test]
    fn compact_definitions_preserve_live_schemas_and_required_parameters() {
        fn assert_schema(full: &Value, compact: &Value) {
            match full {
                Value::Object(object) => {
                    for (key, value) in object {
                        if key == "description" {
                            assert!(compact.get(key).is_none());
                        } else if key == "properties" {
                            let properties = value.as_object().unwrap();
                            assert_eq!(properties.len(), compact[key].as_object().unwrap().len());
                            for (name, property) in properties {
                                assert_schema(property, &compact[key][name]);
                            }
                        } else {
                            assert_schema(value, &compact[key]);
                        }
                    }
                }
                Value::Array(values) => {
                    assert_eq!(values.len(), compact.as_array().unwrap().len());
                    for (index, value) in values.iter().enumerate() {
                        assert_schema(value, &compact[index]);
                    }
                }
                _ => assert_eq!(full, compact),
            }
        }
        for tool in TOOLS {
            let all: Vec<_> = tool.actions.iter().map(|action| action.name).collect();
            for live in [all.as_slice(), &all[..1]] {
                let full = tool.def(live);
                let compact = tool.def_compact(live);
                assert_eq!(full.name, compact.name);
                assert_schema(&full.parameters, &compact.parameters);
                assert!(compact.description.starts_with(tool.purpose));
                assert_eq!(compact.description.lines().count(), live.len() + 1);
                assert!(!compact.description.contains("optional"));
            }
        }
        assert_eq!(
            SCRIPTS.def_compact(&["list", "edit"]).description,
            "Manage confined space scripts, run skill or space scripts, and install packages.\n- action=list\n- action=edit (needs path, edits)"
        );
        assert_eq!(
            FETCH_URL.def_compact(&["fetch_url"]).description,
            "Fetch readable, paged text from URLs, PDFs, or YouTube transcripts, using the 24-hour cache and respecting verifier cache-only mode.\n- fetch_url (needs url)"
        );
        assert!(
            SKILLS.def_compact(&["create"]).parameters["properties"]
                .get("description")
                .is_some()
        );
    }

    // Explicit legacy routing expectations, with each action's required arguments.
    const ROUTES: &[(&str, &str, &str)] = &[
        ("task", r#"{"prompt":"x"}"#, "task"),
        ("skills", r#"{"action":"load","name":"x"}"#, "skill"),
        (
            "skills",
            r#"{"action":"create","name":"x","description":"d"}"#,
            "create_skill",
        ),
        (
            "skills",
            r#"{"action":"install","source":"a/b/c"}"#,
            "install_skill",
        ),
        ("tui_app", r#"{"action":"list"}"#, "tui_app_list"),
        ("tui_app", r#"{"action":"read","name":"x"}"#, "tui_app_read"),
        (
            "tui_app",
            r#"{"action":"write","name":"x","content":"x"}"#,
            "tui_app_write",
        ),
        (
            "tui_app",
            r#"{"action":"edit","name":"x","edits":[{"hash":"h"}]}"#,
            "tui_app_edit",
        ),
        (
            "tui_app",
            r#"{"action":"preview","name":"x"}"#,
            "tui_app_preview",
        ),
        ("tui_app", r#"{"action":"open","name":"x"}"#, "tui_app_open"),
        ("scripts", r#"{"action":"list"}"#, "list_scripts"),
        ("scripts", r#"{"action":"read","path":"x"}"#, "read_script"),
        (
            "scripts",
            r#"{"action":"write","path":"x","content":"x"}"#,
            "write_script",
        ),
        (
            "scripts",
            r#"{"action":"edit","path":"x","edits":[{"hash":"h"}]}"#,
            "edit_script",
        ),
        ("scripts", r#"{"action":"run","path":"x"}"#, "run_script"),
        (
            "scripts",
            r#"{"action":"python","code":"x","name":"x.py"}"#,
            "run_python",
        ),
        (
            "scripts",
            r#"{"action":"install","packages":["x"]}"#,
            "install_packages",
        ),
        ("search", r#"{"mode":"web","query":"x"}"#, "web_search"),
        (
            "search",
            r#"{"mode":"academic","query":"x"}"#,
            "academic_search",
        ),
        (
            "search",
            r#"{"mode":"discussion","query":"x"}"#,
            "discussion_search",
        ),
        (
            "fetch_url",
            r#"{"url":"https://example.test"}"#,
            "fetch_url",
        ),
        ("fs", r#"{"action":"list"}"#, "fs_list"),
        ("fs", r#"{"action":"glob","pattern":"*.rs"}"#, "fs_glob"),
        ("fs", r#"{"action":"grep","pattern":"x"}"#, "fs_grep"),
        ("fs", r#"{"action":"read","path":"x"}"#, "fs_read"),
        (
            "fs",
            r#"{"action":"edit","path":"x","edits":[{"hash":"h"}]}"#,
            "fs_edit",
        ),
        (
            "fs",
            r#"{"action":"write","path":"x","content":""}"#,
            "fs_write",
        ),
        ("fs", r#"{"action":"move","path":"x","to":"y"}"#, "fs_move"),
        ("fs", r#"{"action":"delete","path":"x"}"#, "fs_delete"),
        ("shell", r#"{"action":"run","cmd":"echo hi"}"#, "shell_run"),
        (
            "shell",
            r#"{"action":"log","id":"0123456789ab"}"#,
            "shell_log",
        ),
        (
            "proc",
            r#"{"action":"start","cmd":"sleep 1"}"#,
            "proc_start",
        ),
        ("proc", r#"{"action":"read","id":"0123abcd"}"#, "proc_read"),
        ("proc", r#"{"action":"list"}"#, "proc_list"),
        ("proc", r#"{"action":"kill","id":"0123abcd"}"#, "proc_kill"),
        (
            "research_lookup",
            r#"{"scope":"session_sources","query":"x"}"#,
            "search_sources",
        ),
        (
            "research_lookup",
            r#"{"scope":"citations"}"#,
            "list_citations",
        ),
        (
            "files",
            r#"{"action":"search","query":"x"}"#,
            "search_files",
        ),
        ("files", r#"{"action":"read","name":"x"}"#, "read_file"),
        (
            "files",
            r#"{"action":"pdf_page","name":"x","page":1}"#,
            "read_pdf_page",
        ),
        (
            "media",
            r#"{"action":"generate_image","prompt":"x"}"#,
            "generate_image",
        ),
        (
            "media",
            r#"{"action":"generate_video","prompt":"x"}"#,
            "generate_video",
        ),
        ("media", r#"{"action":"edit","video_id":"x"}"#, "edit_video"),
        (
            "media",
            r#"{"action":"extract_frame","video_id":"x"}"#,
            "extract_frame",
        ),
        (
            "media",
            r#"{"action":"stitch","video_ids":["x"]}"#,
            "stitch_videos",
        ),
        (
            "media",
            r#"{"action":"save_reference","name":"x","image_id":"i","description":"d"}"#,
            "save_reference",
        ),
        (
            "media",
            r#"{"action":"list_references"}"#,
            "list_references",
        ),
        (
            "media",
            r#"{"action":"delete_reference","name":"x"}"#,
            "delete_reference",
        ),
    ];

    #[test]
    fn every_action_param_is_declared() {
        for tool in TOOLS {
            for action in tool.actions {
                for name in action.required.iter().chain(action.optional) {
                    assert!(
                        tool.params.iter().any(|param| param.name == *name),
                        "{}.{} uses undeclared {name}",
                        tool.name,
                        action.name
                    );
                }
            }
        }
    }

    #[test]
    fn retired_table_aliases_migrate_back_through_dispatch() {
        for &(tool, args, internal) in ROUTES {
            if tool == internal {
                continue;
            }
            let (public, selector, mapped) =
                crate::tools::migrate_tool_call(internal, serde_json::from_str(args).unwrap())
                    .unwrap();
            assert_eq!(public, tool);
            assert_eq!(
                mapped[selector],
                serde_json::from_str::<Value>(args).unwrap()[selector]
            );
            let mapped = mapped.to_string();
            assert_eq!(public_call(public, &mapped), Ok((internal.into(), mapped)));
        }
    }

    #[test]
    fn routing_is_unchanged() {
        assert_eq!(
            ROUTES.len(),
            TOOLS.iter().map(|tool| tool.actions.len()).sum::<usize>()
        );
        for &(tool, args, internal) in ROUTES {
            assert_eq!(
                public_call(tool, args),
                Ok((internal.to_string(), args.to_string()))
            );
        }
        for (args, internal) in [
            (r#"{"action":"read","app":"a","path":"x"}"#, "read_app_file"),
            (r#"{"action":"search","app":"a","pattern":"x"}"#, "grep_app"),
            (
                r#"{"action":"write","app":"a","path":"x","content":"x"}"#,
                "write_file",
            ),
            (
                r#"{"action":"patch","app":"a","path":"x","edits":[{"hash":"h"}]}"#,
                "edit_file",
            ),
            (
                r#"{"action":"diff","app":"a","path":"x","content":"x"}"#,
                "diff_app",
            ),
            (r#"{"action":"list"}"#, "list_images"),
            (
                r#"{"action":"copy_file","app":"a","file_name":"x"}"#,
                "copy_file_to_app",
            ),
            (
                r#"{"action":"copy_images","app":"a","image_ids":["x"]}"#,
                "copy_images_to_app",
            ),
            (r#"{"action":"init","app":"a"}"#, "init_app"),
            (r#"{"action":"build","app":"a"}"#, "build_app"),
        ] {
            assert_eq!(
                public_call("app", args),
                Ok((internal.to_string(), args.to_string()))
            );
        }
    }

    #[test]
    fn read_only_classification_is_unchanged() {
        let read_only = [
            "task",
            "skill",
            "web_search",
            "academic_search",
            "discussion_search",
            "fetch_url",
            "search_sources",
            "list_citations",
            "search_files",
            "read_file",
            "read_pdf_page",
            "read_script",
            "list_scripts",
            "tui_app_list",
            "tui_app_read",
            "tui_app_preview",
            "list_references",
            "fs_list",
            "fs_glob",
            "fs_grep",
            "fs_read",
            "shell_log",
            "proc_read",
            "proc_list",
        ];
        for &(tool, args, internal) in ROUTES {
            let expected = read_only.contains(&internal);
            assert_eq!(is_read_only_tool(tool, args), expected, "{tool} {args}");
            assert_eq!(is_read_only_tool(internal, args), expected, "{internal}");
        }
        for action in [
            "read",
            "search",
            "diff",
            "list",
            "write",
            "patch",
            "copy_file",
            "copy_images",
            "init",
            "build",
        ] {
            assert_eq!(
                is_read_only_tool("app", &json!({ "action": action }).to_string()),
                ["read", "search", "diff", "list"].contains(&action),
                "app.{action}"
            );
        }
    }

    #[test]
    fn defs_list_only_params_of_live_actions() {
        let mut toolbox = ToolBox::new(
            std::path::PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        toolbox.image_gen_backend = Some((
            crate::provider::openrouter::OpenRouter::openai(String::new()),
            "test-image-model".to_string(),
        ));
        let media = toolbox
            .defs()
            .into_iter()
            .find(|def| def.name == "media")
            .unwrap();
        assert_eq!(
            media.parameters["properties"]["action"]["enum"],
            json!(["generate_image"])
        );
        let properties = media.parameters["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 4);
        for name in ["action", "prompt", "image_id", "size"] {
            assert!(properties.contains_key(name), "missing {name}");
        }
        assert!(!media.description.contains("action=generate_video"));
        assert!(!media.description.contains("video_id"));
    }

    #[test]
    fn missing_field_errors_say_which_action_needs_it() {
        let error = public_call("scripts", r#"{"action":"python","code":"x"}"#).unwrap_err();
        assert!(error.contains("missing required field: name"), "{error}");
        assert!(error.contains("scripts.python needs it"), "{error}");
        assert!(error.contains("confined .py filename"), "{error}");
        for args in ["{}", r#"{"action":" "}"#] {
            let error = public_call("scripts", args).unwrap_err();
            assert_eq!(
                error,
                "missing required field: action · scripts needs one of: list, read, write, edit, run, python, install"
            );
        }
    }

    #[test]
    fn invalid_action_lists_the_valid_ones() {
        for (tool, args, expected) in [
            (
                "skills",
                r#"{"action":"nope"}"#,
                "invalid action for skills: \"nope\" · valid: load, create, install",
            ),
            (
                "search",
                r#"{"mode":"nope"}"#,
                "invalid mode for search: \"nope\" · valid: web, academic, discussion",
            ),
            (
                "research_lookup",
                r#"{"scope":"nope"}"#,
                "invalid scope for research_lookup: \"nope\" · valid: session_sources, citations",
            ),
        ] {
            assert_eq!(public_call(tool, args).unwrap_err(), expected);
        }
    }

    #[test]
    fn scripts_write_accepts_empty_content() {
        for content in ["", " "] {
            let args = json!({ "action": "write", "path": "x.py", "content": content }).to_string();
            assert_eq!(
                public_call("scripts", &args),
                Ok(("write_script".to_string(), args))
            );
        }
        for args in [
            r#"{"action":"write","path":"x"}"#,
            r#"{"action":"write","path":"x","content":null}"#,
        ] {
            assert!(
                public_call("scripts", args)
                    .unwrap_err()
                    .contains("missing required field: content")
            );
        }
    }

    #[test]
    fn required_fields_preserve_string_array_and_unsigned_integer_checks() {
        for (tool, args, field) in [
            ("scripts", r#"{"action":"read","path":" "}"#, "path"),
            (
                "scripts",
                r#"{"action":"install","packages":[]}"#,
                "packages",
            ),
            (
                "scripts",
                r#"{"action":"edit","path":"x","edits":[]}"#,
                "edits",
            ),
            (
                "files",
                r#"{"action":"pdf_page","name":"x","page":-1}"#,
                "page",
            ),
            (
                "files",
                r#"{"action":"pdf_page","name":"x","page":1.5}"#,
                "page",
            ),
            (
                "files",
                r#"{"action":"pdf_page","name":"x","page":"1"}"#,
                "page",
            ),
            ("fetch_url", r#"{"url":" "}"#, "url"),
        ] {
            assert!(
                public_call(tool, args)
                    .unwrap_err()
                    .contains(&format!("missing required field: {field}"))
            );
        }
        assert!(public_call("files", r#"{"action":"pdf_page","name":"x","page":0}"#).is_ok());
        // Validation has always checked only array presence, not item types.
        assert!(public_call("scripts", r#"{"action":"install","packages":[null]}"#).is_ok());
    }

    #[test]
    fn definitions_keep_required_arrays_and_hash_line_items() {
        for tool in TOOLS {
            let live: Vec<_> = tool.actions.iter().map(|action| action.name).collect();
            let def = tool.def(&live);
            let expected = match tool.name {
                "search" => json!(["mode", "query"]),
                "fetch_url" => json!(["url"]),
                "research_lookup" => json!(["scope"]),
                "task" => json!(["prompt"]),
                _ => json!(["action"]),
            };
            assert_eq!(def.parameters["required"], expected, "{}", tool.name);
        }
        assert_eq!(
            SCRIPTS.def(&["edit"]).parameters["properties"]["edits"]["items"],
            json!({
                "type": "object",
                "properties": {
                    "hash": { "type": "string", "description": "hash from the latest read" },
                    "line": { "type": "integer", "description": "optional 1-based line number to disambiguate repeated lines" },
                    "new": { "type": ["string", "null"] }
                },
                "required": ["hash"]
            })
        );
    }
}
