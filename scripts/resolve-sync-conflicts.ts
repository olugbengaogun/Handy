#!/usr/bin/env bun
/**
 * Resolves the merge conflicts that are not actually questions.
 *
 * The daily upstream sync (`.github/workflows/sync-upstream.yml`) stops and
 * asks a human whenever `git merge upstream/main` collides. Most of those
 * collisions have never been a judgement call:
 *
 *   - `version`/`productName`/`identifier` collide on every single upstream
 *     release, and this fork's numbering is deliberately decoupled, so ours
 *     always wins (see CLAUDE.md).
 *   - `src/bindings.ts` is *generated* by tauri-specta. It collides because
 *     specta packs dozens of fields onto one physical line, so two sides
 *     adding unrelated settings touch the same line. At field granularity
 *     there is no conflict at all.
 *   - Import lists, translation keys and other comma-separated members
 *     collide when both sides append a different entry next to each other.
 *
 * Each of those has one defensible answer that a human would reach every
 * time, so asking is pure latency. Everything else still goes to a human —
 * this script's contract is that it can only ever *shrink* what a human is
 * asked about, never decide something debatable.
 *
 * Run inside a conflicted merge, from the repo root:
 *
 *     bun scripts/resolve-sync-conflicts.ts [--dry-run]
 *
 * Exit 0  every conflicted file was resolved and staged (or would be, under
 *         --dry-run). The caller may commit the merge.
 * Exit 1  at least one conflict needs a human. NOTHING is staged and the
 *         working tree is left exactly as git left it, so the caller's
 *         existing "push a branch and open a PR" path still sees the real,
 *         unmodified conflict.
 *
 * The all-or-nothing contract matters: a half-resolved tree would land a PR
 * whose diff mixes upstream's conflict with this script's edits, and nobody
 * reviewing it could tell which was which.
 */

import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const DRY_RUN = process.argv.includes("--dry-run");

function git(args: string[], opts: { allowFail?: boolean } = {}): string {
  const r = spawnSync("git", args, {
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
  });
  if (r.status !== 0) {
    if (opts.allowFail) return "";
    throw new Error(`git ${args.join(" ")} failed: ${r.stderr?.trim()}`);
  }
  return r.stdout;
}

/** Conflicted paths, as git sees them. */
function conflictedFiles(): string[] {
  return git(["diff", "--name-only", "--diff-filter=U"])
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
}

/**
 * The three sides of a conflict. Stage 1 is the merge base, 2 is ours, 3 is
 * theirs. Read from the index rather than parsed out of the conflict markers
 * in the working file: markers are a *rendering* of the conflict, lossy by
 * design (git picks hunk boundaries for human readability), while the stages
 * are the actual inputs.
 *
 * A missing stage means add/add (no base) or a delete on one side. Those are
 * left to a human — a file one side deleted is never a mechanical merge.
 */
type Stages = { base: string; ours: string; theirs: string };

function readStages(file: string): Stages | null {
  const get = (n: number) => {
    const r = spawnSync("git", ["show", `:${n}:${file}`], {
      encoding: "utf8",
      maxBuffer: 256 * 1024 * 1024,
    });
    return r.status === 0 ? r.stdout : null;
  };
  const base = get(1);
  const ours = get(2);
  const theirs = get(3);
  if (base === null || ours === null || theirs === null) return null;
  return { base, ours, theirs };
}

/**
 * A plain 3-way merge of three strings, returning the merged text or null if
 * it conflicts. Delegates to `git merge-file` rather than reimplementing
 * diff3 — the whole point is to get git's own answer.
 */
function mergeFileRaw(s: Stages): { clean: boolean; text: string } | null {
  const dir = mkdtempSync(join(tmpdir(), "handy-merge-"));
  try {
    const p = (n: string, c: string) => {
      const f = join(dir, n);
      writeFileSync(f, c);
      return f;
    };
    const o = p("ours", s.ours);
    const b = p("base", s.base);
    const t = p("theirs", s.theirs);
    const r = spawnSync(
      "git",
      [
        "merge-file",
        "-p",
        "--diff3",
        "-L",
        "ours",
        "-L",
        "base",
        "-L",
        "theirs",
        o,
        b,
        t,
      ],
      { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 },
    );
    // Exit >= 0 is the number of conflicts left; negative means git itself
    // failed (binary input, unreadable file) and the output means nothing.
    if (r.status === null || r.status < 0) return null;
    return { clean: r.status === 0, text: r.stdout };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** The merged text, or null if the three sides genuinely conflict. */
function mergeFile(s: Stages): string | null {
  const r = mergeFileRaw(s);
  return r && r.clean ? r.text : null;
}

/** The merged text *with* diff3 conflict markers, for shape classification. */
function mergeFileConflicted(s: Stages): string {
  const r = mergeFileRaw(s);
  // An unparseable merge yields no hunks, so every caller treats it as
  // unresolvable — which is the correct answer for input git could not merge.
  return r ? r.text : "";
}

/* ------------------------------------------------------------------ */
/* Hunk-level classification                                           */
/* ------------------------------------------------------------------ */

export type Hunk = { ours: string[]; base: string[]; theirs: string[] };
type Piece = { kind: "text"; lines: string[] } | { kind: "hunk"; hunk: Hunk };

/**
 * Split `git merge-file --diff3` output into literal text and conflict hunks.
 * `--diff3` is what makes any of this safe: without the base section there is
 * no way to tell "both sides added something here" (mergeable) from "both
 * sides rewrote the same thing" (not mergeable), and guessing between those
 * two is exactly how an automated merge eats someone's work.
 */
export function parseDiff3(text: string): Piece[] | null {
  const lines = text.split("\n");
  const pieces: Piece[] = [];
  let acc: string[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.startsWith("<<<<<<<")) {
      acc.push(line);
      i++;
      continue;
    }
    if (acc.length) {
      pieces.push({ kind: "text", lines: acc });
      acc = [];
    }
    const hunk: Hunk = { ours: [], base: [], theirs: [] };
    let section: "ours" | "base" | "theirs" = "ours";
    i++;
    let closed = false;
    for (; i < lines.length; i++) {
      const l = lines[i];
      if (l.startsWith("|||||||")) {
        section = "base";
      } else if (l === "=======") {
        section = "theirs";
      } else if (l.startsWith(">>>>>>>")) {
        i++;
        closed = true;
        break;
      } else {
        hunk[section].push(l);
      }
    }
    // An unterminated hunk means the file contained something that looks like
    // a marker, or merge-file produced output this parser does not understand.
    // Refusing is the only safe answer.
    if (!closed) return null;
    pieces.push({ kind: "hunk", hunk });
  }
  if (acc.length) pieces.push({ kind: "text", lines: acc });
  return pieces;
}

/**
 * Fork identity: the fields whose collision has one answer, forever.
 *
 * This fork's version is deliberately decoupled from upstream's (CLAUDE.md),
 * and its name, bundle identifier and authors are the whole point of the fork
 * existing. Upstream changing any of them is never a change this repo wants —
 * so when every colliding line on both sides is one of these, ours wins with
 * nothing lost.
 */
const IDENTITY_KEYS = new Set([
  "version",
  "productName",
  "identifier",
  "description",
  "authors",
  "name",
]);

/**
 * ...and the only three files that carry it. These are exactly the files the
 * release step rewrites when it bumps a version, which is what makes the list
 * complete rather than merely current.
 *
 * Scoped deliberately. `name`, `version` and `description` are generic enough
 * to appear as an inner key in all sorts of data — `bun.lock` and `flake.lock`
 * are both tracked, both full of name/version pairs, and a collision between
 * two of *those* resolved by "keep ours" would silently drop upstream's entry
 * with nothing to show for it. Identity is a property of these three files, so
 * the rule lives where the property does.
 */
const IDENTITY_FILES = new Set([
  "package.json",
  "src-tauri/Cargo.toml",
  "src-tauri/tauri.conf.json",
]);

/** A JSON `"version": "1.5.1",` or a TOML `version = "1.5.1"` -> `version`. */
function keyOf(line: string): string | null {
  const s = line.trim();
  if (!s) return null;
  const json = s.match(/^"([^"]+)"\s*:/);
  if (json) return json[1];
  const toml = s.match(/^([A-Za-z0-9_-]+)\s*=/);
  if (toml) return toml[1];
  return null;
}

export function isIdentityHunk(h: Hunk): boolean {
  const keys = (ls: string[]) => {
    const out: string[] = [];
    for (const l of ls) {
      if (!l.trim()) continue;
      const k = keyOf(l);
      if (k === null || !IDENTITY_KEYS.has(k)) return null;
      out.push(k);
    }
    return out;
  };
  const ours = keys(h.ours);
  const theirs = keys(h.theirs);
  if (!ours || !theirs || ours.length === 0) return false;
  // Same keys, same multiplicity, on both sides. A side declaring a key the
  // other does not is not two spellings of one thing, and keeping ours would
  // be a silent deletion rather than a resolution.
  const norm = (a: string[]) => [...a].sort().join(" ");
  if (norm(ours) !== norm(theirs)) return false;
  // The base may legitimately be empty (both sides introduced the block), but
  // any base content must be identity too, or real settings have grown around
  // the version line and this is no longer a version-only collision.
  return keys(h.base) !== null;
}

/**
 * Both sides appended a different member to the same list — an import list, a
 * translation-key object, an enum, a struct's fields. Neither deleted anything
 * (the base section is empty) and they added different things, so keeping both
 * is the only resolution that loses nothing.
 *
 * Restricted to lines ending in a comma on purpose. That one syntactic test is
 * what separates "two people added a list member" from "two people wrote a
 * different implementation of the same thing": concatenating the former is
 * right, and concatenating the latter produces plausible nonsense that
 * compiles.
 */
export function isListAppendHunk(h: Hunk): boolean {
  const meaningful = (ls: string[]) => ls.filter((l) => l.trim() !== "");
  const ours = meaningful(h.ours);
  const theirs = meaningful(h.theirs);
  if (meaningful(h.base).length !== 0) return false;
  if (ours.length === 0 || theirs.length === 0) return false;
  const listy = (ls: string[]) => ls.every((l) => l.trim().endsWith(","));
  if (!listy(ours) || !listy(theirs)) return false;
  // Compared by member *name*, not by whole line. Two sides adding the same
  // translation key with different text are two different lines, so a
  // line-wise check would call them disjoint and emit both — and a duplicate
  // JSON key does not fail, it silently keeps the last one. That is a wrong
  // string shipped to users with nothing anywhere reporting it.
  const memberKey = (l: string) => {
    const s = l.trim();
    const named = s.match(/^"([^"]+)"\s*:/);
    if (named) return named[1];
    // A match arm identifies itself by its pattern, not by its body. Two sides
    // adding an arm for the same pattern with different bodies are different
    // lines, and emitting both makes the second unreachable — a warning Rust
    // will not fail on, so it would ship silently doing the first thing.
    const arm = s.indexOf("=>");
    return arm > 0 ? s.slice(0, arm).trim() : s;
  };
  const names = new Set(ours.map(memberKey));
  if (theirs.some((l) => names.has(memberKey(l)))) return false;
  // Overlapping additions would be duplicated by a union, which is a bug and
  // not a merge: a duplicate object key silently wins, a duplicate import
  // fails outright.
  const seen = new Set(ours.map((l) => l.trim()));
  return !theirs.some((l) => seen.has(l.trim()));
}

/**
 * A single-line Rust `use` statement, and nothing else - no attribute, no
 * `pub use` re-export chain spanning lines, no trailing comment.
 */
const USE_LINE = /^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+[^;]+;\s*$/;

/**
 * The module path a `use` line imports *from*: everything between `use` and
 * either the brace list or the terminating semicolon.
 *
 *     use log::{debug, warn};        -> "log"
 *     use crate::utils;              -> "crate::utils"
 *     use std::sync::OnceLock;       -> "std::sync::OnceLock"
 */
function usePathKey(line: string): string {
  const body = useBody(line);
  const brace = body.indexOf("{");
  const head = brace >= 0 ? body.slice(0, brace) : body;
  return head.trim().replace(/::$/, "");
}

function useBody(line: string): string {
  return line
    .trim()
    .replace(/^(?:pub(?:\([^)]*\))?\s+)?use\s+/, "")
    .replace(/;\s*$/, "");
}

/**
 * The names a `use` line binds in the file's namespace - what two imports have
 * to disagree about for the result not to compile:
 *
 *     use log::{debug, warn};   -> ["debug", "warn"]
 *     use crate::utils;         -> ["utils"]
 *     use std::fmt as f;        -> ["f"]
 *
 * `null` for anything this cannot read exactly (a nested group, a `self`
 * re-export), which makes the hunk unmergeable rather than guessed at.
 */
function useBoundNames(line: string): string[] | null {
  const body = useBody(line);
  if ((body.match(/\{/g) || []).length > 1) return null;
  const brace = body.indexOf("{");
  const items =
    brace >= 0
      ? body.slice(brace + 1, body.lastIndexOf("}")).split(",")
      : [body];
  const names: string[] = [];
  for (const raw of items) {
    const item = raw.trim();
    if (!item) continue;
    // `use a::{self, B}` binds the parent module's own name, which is not
    // written in the item. Not worth reconstructing.
    if (item === "self" || item.endsWith("::self")) return null;
    const alias = item.match(/\s+as\s+([A-Za-z_][A-Za-z0-9_]*)$/);
    if (alias) {
      names.push(alias[1]);
      continue;
    }
    const last = item.split("::").pop()?.trim();
    if (!last) return null;
    names.push(last);
  }
  return names.length > 0 ? names : null;
}

/**
 * Both sides edited the same block of `use` statements.
 *
 * This is the most common mechanical collision a long-lived fork has, and it
 * is never a disagreement: `git` merges a *region*, so upstream adding an
 * import on one line and this fork widening the import on the line below are
 * one conflict about nothing. It is what blocked the 2026-08-31 sync alongside
 * the locale file - upstream added `use crate::utils;` while this fork had
 * grown `use log::{debug, warn};` into `use log::{debug, error, warn};`.
 *
 * Unlike `isListAppendHunk` this does not require an empty base, because the
 * interesting case *is* a modified base line. That is safe only because the
 * result is computed as a set operation over the three sides rather than by
 * concatenating them:
 *
 *   - a base line both sides still have  -> kept
 *   - a base line either side dropped    -> dropped (a deletion is a decision)
 *   - a line only one side introduced    -> added
 *
 * which is exactly what a line-wise 3-way merge means, applied to a region git
 * declined to split. Restricted to plain single-line `use` statements so that
 * "the whole hunk is imports" is a syntactic fact and not an inference.
 */
export function isUseBlockHunk(h: Hunk): boolean {
  const sides = [h.ours, h.base, h.theirs];
  // A blank line anywhere means the hunk carries grouping this cannot
  // reproduce, and silently restyling an import block would be a permanent
  // cosmetic divergence from upstream that re-conflicts forever.
  if (sides.some((ls) => ls.some((l) => !USE_LINE.test(l)))) return false;
  if (sides.every((ls) => ls.length === 0)) return false;

  const inBase = new Set(h.base.map((l) => l.trim()));
  const ourAdds = h.ours.filter((l) => !inBase.has(l.trim()));
  // A line both sides introduced identically is not a disagreement - the merge
  // below emits it once - so it is excluded before the collision checks.
  const ourAddText = new Set(ourAdds.map((l) => l.trim()));
  const theirAdds = h.theirs.filter(
    (l) => !inBase.has(l.trim()) && !ourAddText.has(l.trim()),
  );
  // With no additions on either side the hunk is pure deletion, and the set
  // merge below - keep what both sides kept - is exactly right for it; the
  // checks that follow are no-ops in that case.

  // A glob changes what every unqualified name in the file resolves to. Two
  // sides adding imports around one is not a set operation over lines.
  if ([...ourAdds, ...theirAdds].some((l) => l.includes("*"))) return false;

  // Two sides introducing different imports from the same module would emit
  // two `use log::{...}` lines; two introducing the same *name* from different
  // modules would bind it twice. The first is usually harmless and sometimes
  // E0252, the second always is - and neither is a merge this can claim to
  // have done correctly, so both are refused.
  const ourKeys = new Set(ourAdds.map(usePathKey));
  if (theirAdds.some((l) => ourKeys.has(usePathKey(l)))) return false;

  const ourNames = new Set<string>();
  for (const l of ourAdds) {
    const names = useBoundNames(l);
    if (!names) return false;
    for (const n of names) ourNames.add(n);
  }
  for (const l of theirAdds) {
    const names = useBoundNames(l);
    if (!names) return false;
    if (names.some((n) => ourNames.has(n))) return false;
  }

  return true;
}

export function mergeUseBlock(h: Hunk): string[] {
  const inBase = new Set(h.base.map((l) => l.trim()));
  const inOurs = new Set(h.ours.map((l) => l.trim()));
  const inTheirs = new Set(h.theirs.map((l) => l.trim()));
  // Upstream's order is the skeleton, with this fork's own additions appended.
  // Not cosmetic: a block written in ours' order instead would leave the file
  // permanently shuffled relative to cjpais/Handy, and every later upstream
  // edit to those imports would collide with the shuffle. Resolving a conflict
  // by planting the next one is not resolving it.
  return [
    ...h.theirs.filter((l) => inOurs.has(l.trim()) || !inBase.has(l.trim())),
    ...h.ours.filter((l) => !inBase.has(l.trim()) && !inTheirs.has(l.trim())),
  ];
}

/**
 * Resolve a whole file from its diff3 rendering, or return null if any hunk in
 * it is a real question. Per-file all-or-nothing for the same reason the run
 * is: a file half-resolved by machine and half by hand is unreviewable.
 */
export function resolveByShape(merged: string, file: string): string | null {
  const allowIdentity = IDENTITY_FILES.has(file);
  const pieces = parseDiff3(merged);
  if (!pieces) return null;
  // Called only for files git reported as conflicted, so finding no hunk means
  // the merge output was not understood — not that the file is fine. Without
  // this, empty input would "resolve" a file to nothing.
  if (!pieces.some((p) => p.kind === "hunk")) return null;
  const out: string[] = [];
  for (const p of pieces) {
    if (p.kind === "text") {
      out.push(...p.lines);
      continue;
    }
    if (allowIdentity && isIdentityHunk(p.hunk)) {
      out.push(...p.hunk.ours);
    } else if (isListAppendHunk(p.hunk)) {
      out.push(...p.hunk.ours, ...p.hunk.theirs);
    } else if (isUseBlockHunk(p.hunk)) {
      out.push(...mergeUseBlock(p.hunk));
    } else {
      return null;
    }
  }
  return out.join("\n");
}

/* ------------------------------------------------------------------ */
/* src/bindings.ts — merged at field granularity, not line granularity  */
/* ------------------------------------------------------------------ */

/**
 * `src/bindings.ts` is generated by tauri-specta from the Rust types, and its
 * generator packs every field that has no doc comment onto one physical line:
 *
 *     reliable_paste?: boolean; typing_tool?: TypingTool; external_script...
 *
 * So this fork adding `audio_normalization` and upstream adding `vad_backend`
 * are, to git, two edits to the same 1,259-character line — a conflict. At
 * field granularity they do not overlap at all. Every `bindings.ts` collision
 * this fork has ever seen is this artifact, not a disagreement.
 *
 * Splitting on the field separator turns the artifact back into what it
 * actually is, and the join is the exact inverse, so the committed file stays
 * byte-for-byte in upstream's generated format. Reformatting it instead would
 * be a permanent divergence that collides on every future merge — the same
 * trap the version-bump `sed` calls in sync-upstream.yml avoid.
 */
/**
 * Physical line breaks are carried as an explicit sentinel rather than
 * inferred, so the split is plain concatenation and the join is its exact
 * inverse. The sentinel cannot occur in generated TypeScript, and its absence
 * from all three inputs is asserted before anything is merged.
 */
const EOL_SENTINEL = "␞<<handy-line-break>>";

/**
 * One fragment per field, splitting *after* each semicolon.
 *
 * Splitting on `"; "` instead looks more natural and is subtly wrong: specta
 * emits a trailing space after the last field on a line only when a
 * doc-commented field follows, so a field can gain or lose that space purely
 * because of what was added *after* it. Splitting on the separator folds that
 * space into the field's own fragment, making an unchanged field look edited
 * on both sides — a conflict manufactured entirely by the representation.
 * Splitting after the semicolon leaves the field fragment byte-identical and
 * isolates the space as its own trivially-mergeable fragment.
 */
function splitFields(text: string): string[] {
  const out: string[] = [];
  for (const line of text.split("\n")) {
    out.push(...line.split(/(?<=;)/));
    out.push(EOL_SENTINEL);
  }
  return out;
}

function joinFields(lines: string[]): string {
  const out: string[] = [];
  let cur = "";
  for (const l of lines) {
    if (l === EOL_SENTINEL) {
      out.push(cur);
      cur = "";
    } else {
      cur += l;
    }
  }
  // Reachable only if the merged text ended without a final sentinel, which
  // the round-trip check turns into a refusal rather than a silent truncation.
  if (cur !== "") out.push(cur);
  // The empty tail that a newline-terminated file produces is kept on purpose:
  // it is what restores the file's final newline when these are rejoined.
  return out.join("\n");
}

/**
 * Every identifier the bindings declare: `foo?: T` fields and `export type
 * Foo` names. Used to prove the merge kept what both sides meant to keep.
 */
function identifiers(text: string): Set<string> {
  const out = new Set<string>();
  for (const m of text.matchAll(/(?:^|[;{\s])([A-Za-z_][A-Za-z0-9_]*)\?:/g))
    out.add("field:" + m[1]);
  for (const m of text.matchAll(/^export type ([A-Za-z0-9_]+)/gm))
    out.add("type:" + m[1]);
  for (const m of text.matchAll(/^\s*async ([A-Za-z0-9_]+)\(/gm))
    out.add("cmd:" + m[1]);
  return out;
}

/**
 * Resolve the conflicts left in a *generated* file, where every hunk is
 * necessarily an add/add: the file is a projection of the Rust types, so two
 * sides can only ever have added declarations to it. The base section being
 * empty is checked rather than assumed — a non-empty base means one side
 * changed or removed something that already existed, which is a real edit and
 * not this function's business.
 */
function resolveGeneratedShape(merged: string): string | null {
  const pieces = parseDiff3(merged);
  if (!pieces) return null;
  if (!pieces.some((p) => p.kind === "hunk")) return null;
  const out: string[] = [];
  const startsWith = (a: string[], b: string[]) =>
    b.every((l, i) => a[i] === l);
  const endsWith = (a: string[], b: string[]) =>
    b.every((l, i) => a[a.length - b.length + i] === l);
  for (const p of pieces) {
    if (p.kind === "text") {
      out.push(...p.lines);
      continue;
    }
    const { ours, base, theirs } = p.hunk;
    if (base.some((l) => l.trim() !== "")) return null;
    // One side's addition containing the other's means they agreed on the
    // shared part and one went further — taking the longer keeps the result
    // byte-identical to what the generator emits. Concatenating instead would
    // duplicate the shared lines, and cosmetic drift in a generated file
    // re-conflicts on every future merge.
    if (startsWith(theirs, ours) || endsWith(theirs, ours)) out.push(...theirs);
    else if (startsWith(ours, theirs) || endsWith(ours, theirs))
      out.push(...ours);
    else out.push(...ours, ...theirs);
  }
  return out.join("\n");
}

function mergeGenerated(s: Stages): string | null {
  // Refuse unless the transform is provably lossless on all three inputs. A
  // generator whose output format has changed shape must reach a human, not be
  // guessed at.
  for (const side of [s.base, s.ours, s.theirs]) {
    if (side.includes(EOL_SENTINEL)) return null;
    if (joinFields(splitFields(side)) !== side) return null;
  }
  const split: Stages = {
    base: splitFields(s.base).join("\n"),
    ours: splitFields(s.ours).join("\n"),
    theirs: splitFields(s.theirs).join("\n"),
  };
  const clean = mergeFile(split);
  const mergedSplit =
    clean !== null ? clean : resolveGeneratedShape(mergeFileConflicted(split));
  if (mergedSplit === null) return null;
  const result = joinFields(mergedSplit.split("\n"));

  // The merge of two additive sides must keep: everything both sides still
  // have (neither removed it), plus each side's own additions. Stated over the
  // base rather than as a plain superset so that a field upstream genuinely
  // *deleted* is allowed to disappear, while one lost to a bad merge is not.
  const [b, o, t, r] = [s.base, s.ours, s.theirs, result].map(identifiers);
  const required = new Set<string>();
  for (const k of o) if (!b.has(k) || t.has(k)) required.add(k);
  for (const k of t) if (!b.has(k) || o.has(k)) required.add(k);
  for (const k of required) if (!r.has(k)) return null;
  return result;
}

/* ------------------------------------------------------------------ */
/* Driver                                                              */
/* ------------------------------------------------------------------ */

/**
 * Cargo.lock is generated too, but by a resolver rather than a formatter, so
 * text-merging it is meaningless: the correct file is whatever `cargo` derives
 * from the merged `Cargo.toml`. The caller regenerates it instead — this
 * script only reports that it needs to.
 */
const REGENERATED = new Set(["src-tauri/Cargo.lock"]);

function main(): number {
  // Every path git hands back is relative to the repository root, and this
  // script writes those paths straight back out. Run from anywhere else, that
  // silently creates a parallel tree of half-merged files somewhere it was
  // never meant to touch, so the root is adopted rather than assumed.
  process.chdir(git(["rev-parse", "--show-toplevel"]).trim());

  const files = conflictedFiles();
  if (files.length === 0) {
    // Reached only if the caller ran this outside a conflicted merge. Saying
    // "nothing to do" would read as "all resolved" to a caller about to
    // commit, so it is an error rather than a shrug.
    console.log("Not in a conflicted merge — nothing to resolve.");
    return 1;
  }

  const resolved: Array<[string, string]> = [];
  const regenerate: string[] = [];
  const unresolved: string[] = [];

  for (const file of files) {
    if (REGENERATED.has(file)) {
      regenerate.push(file);
      continue;
    }
    const stages = readStages(file);
    if (!stages) {
      // A missing stage is an add/add or a delete on one side. Neither is a
      // mechanical merge.
      unresolved.push(file);
      continue;
    }
    let text: string | null;
    if (file === "src/bindings.ts") {
      text = mergeGenerated(stages);
    } else {
      const clean = mergeFile(stages);
      text =
        clean !== null
          ? clean
          : resolveByShape(mergeFileConflicted(stages), file);
    }
    if (text === null) unresolved.push(file);
    else resolved.push([file, text]);
  }

  for (const f of resolved) console.log(`  resolved   ${f[0]}`);
  for (const f of regenerate) console.log(`  regenerate ${f}`);
  for (const f of unresolved) console.log(`  needs you  ${f}`);

  if (unresolved.length > 0) {
    console.log(
      `\n${unresolved.length} conflict(s) need a human; nothing was changed.`,
    );
    return 1;
  }

  if (DRY_RUN) {
    console.log("\n--dry-run: would resolve everything above.");
    return 0;
  }

  for (const [file, text] of resolved) {
    writeFileSync(file, text);
    git(["add", "--", file]);
  }
  // Machine-readable, for the workflow step that owns `cargo`.
  for (const file of regenerate) console.log(`REGENERATE=${file}`);
  return 0;
}

// Guarded so the pure classifiers above can be imported by
// resolve-sync-conflicts.test.ts without the script trying to resolve a merge
// that is not in progress. `bun scripts/resolve-sync-conflicts.ts`, which is
// how sync-upstream.yml invokes it, still runs exactly as before.
if (import.meta.main) {
  process.exit(main());
}
