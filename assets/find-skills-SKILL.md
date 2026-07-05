---
name: find-skills
description: Find and install new skills from GitHub when the user asks for a capability you don't have.
---
Skills are directories on GitHub containing a `SKILL.md` (frontmatter with
`name` and `description`, then instructions). You can search for them and
install them yourself.

1. Search with the `web_search` tool. Good queries: `github SKILL.md <topic>`
   or `github "claude skill" <topic>`. Known collections worth checking first:
   `anthropics/skills` (one skill per top-level directory, e.g.
   `anthropics/skills/pdf`).
2. Turn a result URL like `https://github.com/<owner>/<repo>/tree/<branch>/<path>`
   into the install source `<owner>/<repo>/<path>` (drop `tree/<branch>`; a
   skill at the repo root is just `<owner>/<repo>`).
3. Tell the user which skill you found and what it does, then call
   `install_skill` with that source.
4. On success the skill is immediately usable: load it with the `skill` tool.

Installed skills may ship scripts — run them with the `run_script` tool.
Python scripts get the skill's own virtualenv (its `requirements.txt` installs
automatically); add extra packages with `install_packages(skill, packages)`.

If the user names a repo or source directly, skip the search and install it.
If installation fails with "no SKILL.md", the path doesn't point at a skill
directory — check the repo layout and try the correct subdirectory.
