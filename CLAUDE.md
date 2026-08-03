Read @AGENTS.md

## Fork-specific guidance

This is "Handy Plus" — a personal fork of `cjpais/Handy` (upstream, remote name `upstream`) that intentionally diverges (rebrand, extra features) but still wants to keep merging in upstream's improvements via a low-effort `git fetch upstream && git merge` workflow, including a daily automated sync (`.github/workflows/sync-upstream.yml`) that auto-merges when clean and only opens a PR when something collides.

**Before implementing any nontrivial change here, actively consider whether it increases future upstream-merge conflict risk** — e.g. broad rewrites of files CJ actively maintains, or restructuring shared code paths he's likely to also touch. If a change would make merges messier than necessary, **push back and propose the lower-conflict alternative** (additive settings fields/DB columns, wrapper functions, new isolated files/components) rather than silently implementing the riskier version. This is a standing instruction from the repo owner.

**Versioning:** as of this fork, `version` in `tauri.conf.json`/`Cargo.toml`/`package.json` is on its own line (`1.0.0+`), intentionally decoupled from upstream's `0.x` numbering so users can't confuse a Handy Plus build with vanilla Handy. Version bumps for this fork's own releases are done deliberately, not inherited automatically from an upstream merge — the daily sync may occasionally flag a real (but low-stakes) conflict on just that one line when CJ bumps his own version; when that happens, keep this fork's version, don't take upstream's.

**Before cutting a release, check what is actually published — never trust the local version number.** This fork's version is not chosen by hand alone: the daily upstream sync bumps it and cuts a `chore: release vX.Y.Z (automated upstream sync)` commit whenever CJ publishes his own release. So a working copy that has not been pulled recently reports a version that has _already shipped_, and a release cut from it either collides with an existing tag or hands users a build older than the one they are running. Always:

```bash
git fetch origin --tags
git tag --list 'v[0-9]*' | sed 's/^v//' | sort -V | tail -1   # what is really out there
git rev-list --left-right --count origin/main...HEAD          # left must be 0
```

then pick the next version above that. `release.yml` enforces this — it refuses to run when the version in `tauri.conf.json` is not strictly newer than the highest published tag — but the check exists to catch the mistake, not to excuse making it. Note also that the version being ahead of the last _local_ tag proves nothing; only the remote tag list counts.

Additional local-only working instructions (release-rerun policy, automated bug-sweep and plan-upgrade triggers, commit-authorship rule) live in `CLAUDE.local.md`, which is git-ignored and never pushed.
