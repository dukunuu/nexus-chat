---
name: find-skills
description: Find and install Agent Skills when a task needs a specialized capability that is not already available.
---
# Find Skills

Skills follow the Agent Skills convention: each skill is a directory containing
`SKILL.md` with YAML frontmatter (`name`, `description`) and an instruction
body. Nexus discovers app, project, and user/global skill roots and loads only
metadata at startup. Do not assume a skill is usable until its `SKILL.md` has
been loaded successfully.

When the user's request needs a specialized capability that is not already
covered by an available skill:

1. Search with the `search` tool using `mode=web`. Useful queries include
   `github SKILL.md <topic>` and `github "agent skills" <topic>`. Check
   established collections such as `anthropics/skills` first.
2. Convert a result URL like
   `https://github.com/<owner>/<repo>/tree/<branch>/<path>` to the install
   source `<owner>/<repo>/<path>` (drop `tree/<branch>`). A root skill uses
   `<owner>/<repo>`.
3. Tell the user which skill you found and what it does, then call `skills`
   with `action=install` and that source.
4. After a successful install, call `skills` with `action=load` and the skill
   name before using it.

If the user names a repository or source directly, skip the search and install
it. If installation fails with `no SKILL.md`, the source points at a repository
or parent directory rather than a skill directory; inspect the repository
layout and retry with the exact skill path.

Skills can include resource files and scripts. Load a specific resource with
`skills(action=load, name=<skill>, file=<relative-path>)`; run a bundled script
only with `scripts(action=run, skill=<skill>, path=<relative-path>)`. Python
scripts use the skill's own virtual environment, and packages belong in that
skill environment via `scripts(action=install, skill=<skill>, packages=[...])`.
Keep all paths relative to the skill directory and never invent missing
resources.
