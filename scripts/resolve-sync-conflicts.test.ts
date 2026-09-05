// Standalone assert check (no JS unit-test runner in this repo). Run with:
//   bun scripts/resolve-sync-conflicts.test.ts
//
// Covers the hunk classifiers, which are the part of the resolver that decides
// what a human never has to look at. Everything here is pure: no git, no merge
// in progress, no filesystem.
import assert from "node:assert";
import {
  isUseBlockHunk,
  mergeUseBlock,
  isListAppendHunk,
  isIdentityHunk,
  type Hunk,
} from "./resolve-sync-conflicts";

const hunk = (ours: string[], base: string[], theirs: string[]): Hunk => ({
  ours,
  base,
  theirs,
});

// --------------------------------------------------------- use-block merging

// The real hunk that blocked the 2026-08-31 sync: upstream added an import,
// this fork had widened the one next to it, and git merged the whole region.
{
  const h = hunk(
    ["use log::{debug, error, warn};"],
    ["use log::{debug, warn};"],
    ["use crate::utils;", "use log::{debug, warn};"],
  );
  assert.ok(isUseBlockHunk(h));
  // Upstream's order, with this fork's widened line in the slot the one it
  // replaced occupied - byte-identical to what a careful human writes.
  assert.deepStrictEqual(mergeUseBlock(h), [
    "use crate::utils;",
    "use log::{debug, error, warn};",
  ]);
}

// A line both sides left alone survives exactly once.
{
  const h = hunk(
    ["use std::fmt;", "use crate::a::A;"],
    ["use std::fmt;"],
    ["use std::fmt;", "use crate::b::B;"],
  );
  assert.ok(isUseBlockHunk(h));
  assert.deepStrictEqual(mergeUseBlock(h), [
    "use std::fmt;",
    "use crate::b::B;",
    "use crate::a::A;",
  ]);
}

// A deletion is a decision, not an omission: an import either side dropped
// must not be resurrected by the other side still carrying it.
{
  const h = hunk(
    ["use std::fmt;"],
    ["use std::fmt;", "use std::io::Read;"],
    ["use std::fmt;", "use std::io::Read;", "use crate::x::X;"],
  );
  assert.ok(isUseBlockHunk(h));
  assert.deepStrictEqual(mergeUseBlock(h), [
    "use std::fmt;",
    "use crate::x::X;",
  ]);
}

// Both sides adding the identical import yields it once, not twice.
{
  const h = hunk(["use std::fmt;"], [], ["use std::fmt;"]);
  assert.ok(isUseBlockHunk(h));
  assert.deepStrictEqual(mergeUseBlock(h), ["use std::fmt;"]);
}

// Two different braced imports from the *same* module would emit two
// `use log::{...}` lines - the same name imported twice, which does not
// compile. Refused rather than merged.
assert.strictEqual(
  isUseBlockHunk(
    hunk(
      ["use log::{debug, error};"],
      ["use log::{debug};"],
      ["use log::{debug, warn};"],
    ),
  ),
  false,
);

// Different modules binding the *same* name is E0252 just as surely, and the
// module-path check above cannot see it.
assert.strictEqual(
  isUseBlockHunk(hunk(["use crate::a::Foo;"], [], ["use crate::b::Foo;"])),
  false,
);

// ...but the same name behind different aliases is fine.
{
  const h = hunk(
    ["use crate::a::Foo;"],
    [],
    ["use crate::b::Foo as OtherFoo;"],
  );
  assert.ok(isUseBlockHunk(h));
  assert.deepStrictEqual(mergeUseBlock(h), [
    "use crate::b::Foo as OtherFoo;",
    "use crate::a::Foo;",
  ]);
}

// A shape this cannot read exactly is refused rather than guessed at. The
// unclosed group matters most: without an explicit guard the brace slice
// produces garbage member names, and garbage names make the duplicate-import
// checks pass on input they should have refused.
for (const unreadable of [
  "use a::{b::{c, d}, e};",
  "use a::{self, B};",
  "use a::{b;",
]) {
  assert.strictEqual(
    isUseBlockHunk(hunk([unreadable], [], ["use crate::z::Z;"])),
    false,
    `expected refusal for: ${unreadable}`,
  );
}

// Both sides deleting different imports is a pure-deletion hunk, and keeping
// what both sides kept is exactly right for it.
{
  const h = hunk(
    ["use std::fmt;", "use std::io::Read;"],
    ["use std::fmt;", "use std::io::Read;", "use std::io::Write;"],
    ["use std::fmt;", "use std::io::Write;"],
  );
  assert.ok(isUseBlockHunk(h));
  assert.deepStrictEqual(mergeUseBlock(h), ["use std::fmt;"]);
}

// A glob changes what every unqualified name in the file resolves to, so a
// hunk introducing one is not a set operation over lines.
assert.strictEqual(
  isUseBlockHunk(hunk(["use crate::a::*;"], [], ["use crate::b::B;"])),
  false,
);

// Anything that is not a plain single-line `use` disqualifies the whole hunk -
// an attribute, a function, a blank line carrying grouping this cannot
// reproduce without permanently restyling the block away from upstream.
for (const stray of [
  '#[cfg(target_os = "macos")]',
  "pub fn thing() {",
  "",
  "use std::fmt; // keep",
]) {
  assert.strictEqual(
    isUseBlockHunk(hunk(["use crate::a::A;", stray], [], ["use crate::b::B;"])),
    false,
    `expected refusal for stray line: ${JSON.stringify(stray)}`,
  );
}

// An entirely empty hunk resolves to nothing, which would silently delete the
// region. It must be refused, not "merged".
assert.strictEqual(isUseBlockHunk(hunk([], [], [])), false);

// `pub use` re-exports are still plain use lines and merge the same way.
{
  const h = hunk(["pub use crate::a::A;"], [], ["pub use crate::b::B;"]);
  assert.ok(isUseBlockHunk(h));
  assert.deepStrictEqual(mergeUseBlock(h), [
    "pub use crate::b::B;",
    "pub use crate::a::A;",
  ]);
}

// ------------------------------------------- the rules this one sits beside
//
// Guards against the new classifier widening either of the existing two: a
// hunk that was refused before must still be refused, for the same reason.

// Two sides writing a different implementation of the same thing is the case
// isListAppendHunk exists to refuse. isUseBlockHunk must not pick it up.
{
  const h = hunk(
    ["    self.value = compute_fast();"],
    [],
    ["    self.value = compute_exact();"],
  );
  assert.strictEqual(isListAppendHunk(h), false);
  assert.strictEqual(isUseBlockHunk(h), false);
}

// Version lines are still identity, not imports.
{
  const h = hunk(
    ['  "version": "1.5.2",'],
    ['  "version": "1.5.1",'],
    ['  "version": "0.9.7",'],
  );
  assert.ok(isIdentityHunk(h));
  assert.strictEqual(isUseBlockHunk(h), false);
}

console.log("resolve-sync-conflicts: all assertions passed");
