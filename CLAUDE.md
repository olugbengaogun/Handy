Read @AGENTS.md

## Fork-specific guidance

This is "Handy Plus" — a personal fork of `cjpais/Handy` (upstream, remote name `upstream`) that intentionally diverges (rebrand, extra features) but still wants to keep merging in upstream's improvements via a low-effort `git fetch upstream && git merge` workflow, including a daily automated sync (`.github/workflows/sync-upstream.yml`) that auto-merges when clean and only opens a PR when something collides.

**Before implementing any nontrivial change here, actively consider whether it increases future upstream-merge conflict risk** — e.g. broad rewrites of files CJ actively maintains, or restructuring shared code paths he's likely to also touch. If a change would make merges messier than necessary, **push back and propose the lower-conflict alternative** (additive settings fields/DB columns, wrapper functions, new isolated files/components) rather than silently implementing the riskier version. This is a standing instruction from the repo owner.
