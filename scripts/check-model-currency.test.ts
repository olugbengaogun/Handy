/**
 * Tests for the pure helpers in check-model-currency.ts.
 *
 * The check itself is mostly network, and the module is deliberately written so
 * the parts that decide things are importable without firing any of it — the
 * comment above its entry point has said so since it was written, but nothing
 * ever took it up. What is guarded here is the logic that turns findings into a
 * job status, because that is where this check has actually gone wrong: for
 * weeks it failed the run on findings it had itself labelled informational.
 *
 * Run: bun scripts/check-model-currency.test.ts
 */

import assert from "node:assert/strict";
import {
  catalogRepoIds,
  compareVersions,
  exitCodeFor,
  hasActionableFinding,
  orgDriftFindings,
  pinnedCrateVersion,
  type Finding,
} from "./check-model-currency";

const info = (check: string): Finding => ({
  check,
  severity: "info",
  message: "context, not a call to action",
});
const warn = (check: string): Finding => ({
  check,
  severity: "warn",
  message: "something to do",
});

// ── the severity gate ────────────────────────────────────────────────────────

assert.equal(hasActionableFinding([]), false, "nothing found is not a failure");
assert.equal(
  hasActionableFinding([info("shipping-drift"), info("catalog-drift")]),
  false,
  "informational findings must not fail the job — the regression that made " +
    "a missing release tag and an upstream fetch blip look like real drift",
);
assert.equal(
  hasActionableFinding([info("catalog-drift"), warn("crate-pin")]),
  true,
  "one actionable finding among informational ones still fails",
);
assert.equal(hasActionableFinding([warn("org-drift")]), true);

// ── the exit-code contract ───────────────────────────────────────────────────

assert.equal(exitCodeFor({ incomplete: false, actionable: false }), 0);
assert.equal(exitCodeFor({ incomplete: false, actionable: true }), 2);
assert.equal(
  exitCodeFor({ incomplete: true, actionable: false }),
  1,
  "a sub-check that could not reach the network is a failure to look",
);
assert.equal(
  exitCodeFor({ incomplete: true, actionable: true }),
  1,
  "incomplete outranks actionable: with a hole in the data we must not " +
    "announce drift, which would name the failure wrongly all over again",
);

// ── the org-drift grace period ───────────────────────────────────────────────

assert.deepEqual(orgDriftFindings([], 30), [], "no missing models, no finding");

const fresh = orgDriftFindings([{ slug: "granite-5.0", ageDays: 12 }], 30);
assert.equal(fresh.length, 1, "a fresh model is still reported");
assert.equal(
  fresh[0].severity,
  "info",
  "inside the grace period this is upstream's normal batching cadence",
);
assert.match(fresh[0].message, /published 12d ago/);

const stale = orgDriftFindings([{ slug: "granite-5.0", ageDays: 31 }], 30);
assert.equal(
  stale[0].severity,
  "warn",
  "past the grace period upstream has genuinely stalled",
);

// The boundary itself: `>= graceDays`, so day 30 is already a stall. Asserted
// because an off-by-one here is silent — it just delays the alarm by a week.
assert.equal(
  orgDriftFindings([{ slug: "m", ageDays: 29 }], 30)[0].severity,
  "info",
);
assert.equal(
  orgDriftFindings([{ slug: "m", ageDays: 30 }], 30)[0].severity,
  "warn",
);

// One stalled model among fresh ones is enough to escalate, and every model is
// listed either way — the report should never hide what it is reasoning about.
const mixed = orgDriftFindings(
  [
    { slug: "fresh-one", ageDays: 3 },
    { slug: "stalled-one", ageDays: 90 },
  ],
  30,
);
assert.equal(mixed[0].severity, "warn");
assert.match(mixed[0].message, /fresh-one/);
assert.match(mixed[0].message, /stalled-one/);

// An unreadable creation date must never escalate on its own: one flaky Hub
// response would otherwise turn the watchdog red for a reason nobody can act on.
const unknown = orgDriftFindings([{ slug: "mystery", ageDays: null }], 30);
assert.equal(unknown[0].severity, "info");
assert.match(unknown[0].message, /publication date unavailable/);
assert.equal(
  orgDriftFindings(
    [
      { slug: "mystery", ageDays: null },
      { slug: "old", ageDays: 400 },
    ],
    30,
  )[0].severity,
  "warn",
  "but it must not mask a model that is provably stalled",
);

// ── the parsing helpers ──────────────────────────────────────────────────────

assert.equal(
  pinnedCrateVersion(
    'transcribe-cpp = { version = "0.2.3", default-features = false }',
    "transcribe-cpp",
  ),
  "0.2.3",
);
assert.equal(
  pinnedCrateVersion(
    "[target.'cfg(target_os = \"macos\")'.dependencies]\n" +
      'transcribe-cpp = { version = "0.2.3", features = ["metal"] }',
    "transcribe-cpp",
  ),
  "0.2.3",
  "the first pin wins even when it is not on line one",
);
assert.equal(pinnedCrateVersion('serde = "1"', "transcribe-cpp"), null);

assert.ok(compareVersions("0.3.0", "0.2.3") > 0);
assert.ok(compareVersions("0.2.3", "0.2.3") === 0);
assert.ok(compareVersions("0.2.3", "0.2.10") < 0, "numeric, not lexical");
assert.ok(compareVersions("1.0", "1.0.0") === 0, "missing parts read as zero");

assert.deepEqual(
  catalogRepoIds(
    JSON.stringify({
      models: [
        { id: "handy-computer/whisper-small-gguf" },
        { id: "handy-computer/parakeet-tdt-0.6b-v3-gguf" },
      ],
    }),
  ),
  [
    "handy-computer/whisper-small-gguf",
    "handy-computer/parakeet-tdt-0.6b-v3-gguf",
  ],
);
assert.deepEqual(
  catalogRepoIds(JSON.stringify({})),
  [],
  "an empty catalog is not a crash",
);

console.log("check-model-currency: all assertions passed");
