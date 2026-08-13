#!/usr/bin/env bash
# Generate GitHub release notes from the conventional commits between the
# previous release tag and the one being cut.
#
# This project commits straight to master (no PRs), so GitHub's PR-based
# auto-generated notes come out empty ("no changes"). This script walks the
# git history instead, groups commits by conventional prefix (feat/fix/
# docs/…) and prints ready-to-post markdown to stdout.
#
# Usage: scripts/release-notes.sh [tag] [previous-tag]
# Both default from git: the nearest tag reachable from HEAD, then the
# nearest tag behind it. Version-bump commits ("chore: bump to X.Y.Z") are
# skipped as noise.

set -euo pipefail
cd "$(dirname "$0")/.."

repo="$(sed -n 's/^repository = "\(.*\)"/\1/p' Cargo.toml | head -1)"

tag="${1:-}"
prev="${2:-}"
if [ -z "$tag" ]; then
    tag="$(git describe --tags --abbrev=0 2>/dev/null || true)"
fi
if [ -z "$prev" ] && [ -n "$tag" ]; then
    prev="$(git describe --tags --abbrev=0 "$tag^" 2>/dev/null || true)"
fi
if [ -z "$tag" ]; then
    echo "release-notes: no tags found — push a tag first" >&2
    exit 1
fi

range="$tag"
[ -n "$prev" ] && range="$prev..$tag"
date="$(git log -1 --format=%cs "$tag")"

{
    echo "## What's changed in $tag${date:+ ($date)}"
    echo
    if [ -n "$prev" ]; then
        echo "Commits since [$prev]($repo/compare/$prev...$tag):"
    else
        echo "Initial release — the full history."
    fi
    echo
    # One line per commit: "<hash> <subject>". $1 is the hash; the
    # subject is the rest of the line, spaces and tabs intact.
    git log --no-merges --format='%h %s' "$range" |
        awk -v repo="$repo" '
            BEGIN {
                order["feat"] = 1; order["fix"] = 2; order["design"] = 3
                order["docs"] = 4; order["refactor"] = 5; order["style"] = 5
                order["test"] = 6; order["perf"] = 7
                order["build"] = 8; order["chore"] = 8; order["license"] = 8
                order["ci"] = 8
                names[1] = "Features"; names[2] = "Fixes"; names[3] = "Design"
                names[4] = "Docs"; names[5] = "Refactors"; names[6] = "Tests"
                names[7] = "Performance"; names[8] = "Maintenance"; names[9] = "Other"
            }
            {
                hash = $1
                subject = $0
                sub(/^[^ ]+ /, "", subject)
                # The version bump commit is workflow noise, not a change.
                if (subject ~ /^chore: bump to v?[0-9]/) next
                if (subject ~ /^[a-z]+(\([^)]*\))?: /) {
                    match(subject, /^[a-z]+/)
                    prefix = substr(subject, 1, RLENGTH)
                } else {
                    prefix = ""
                }
                sec = (prefix in order) ? order[prefix] : 9
                n[sec]++
                lines[sec, n[sec]] = "- " subject " ([" hash "](" repo "/commit/" hash "))"
            }
            END {
                empty = 1
                for (sec = 1; sec <= 9; sec++) {
                    if (n[sec] == 0) continue
                    if (!empty) print ""
                    empty = 0
                    print "### " names[sec]
                    for (i = 1; i <= n[sec]; i++) print lines[sec, i]
                }
                if (empty) print "No notable changes."
            }
        '
}
