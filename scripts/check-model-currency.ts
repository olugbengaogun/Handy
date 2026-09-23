/**
 * Model currency check for Handy Plus.
 *
 * Handy Plus is a fork of cjpais/Handy and inherits its model catalog through a
 * daily upstream sync. Upstream currently maintains that catalog aggressively —
 * which is exactly why we should NOT build a parallel model pipeline. What we
 * need instead is to notice quickly if any link in the delivery chain stalls:
 *
 *     handy-computer publishes a model
 *       -> upstream regenerates catalog.json      (check 1, check 2)
 *       -> our daily sync merges it               (check 1)
 *       -> the arch is supported by transcribe-cpp (check 3)
 *       -> WE CUT A RELEASE                       (check 4)  <- most likely to stall
 *       -> the user updates and sees the model
 *
 * The catalog is `include_str!`-baked into the binary and runtime discovery is
 * local-only (custom models dir + HF cache), so a model that is merged but not
 * released is invisible to users. That makes check 4 the one that matters most,
 * and the one nobody remembers to do by hand.
 *
 * Exit codes are the contract, because the workflow used to decide the job's
 * fate by grepping this script's prose out of a `tee` pipeline. That had two
 * faults. `tee` returns its own status and GitHub's shell sets no `pipefail`,
 * so a crash here exited 0 and was then reported as "drift" — a misdiagnosis
 * of the single failure that most needs its real name. And the gate keyed on
 * an English sentence, so rewording a line silently inverted it.
 *
 *     0  no actionable finding (informational ones may still be reported)
 *     1  the check itself could not complete — it threw, or one of its
 *        sub-checks could not reach HuggingFace or crates.io. This outranks 2:
 *        a run that could not look has not found anything, and reporting it as
 *        drift would repeat the very fault these codes exist to remove.
 *     2  at least one `warn` finding — something this fork should act on
 *
 * Severity is the other half of that contract. Every finding carries one, and
 * only `warn` is actionable; `info` exists to say "this was looked at and is
 * fine" without turning the run red. Nothing consumed that distinction before,
 * so "No release tag found; skipping" and "upstream fetch failed" — both
 * deliberately informational — failed the job exactly like real drift did.
 *
 * Usage:  bun run scripts/check-model-currency.ts [--json]
 */

import { readFileSync } from "node:fs";
import { execSync } from "node:child_process";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CATALOG_PATH = resolve(REPO_ROOT, "src-tauri/src/catalog/catalog.json");
const CARGO_PATH = resolve(REPO_ROOT, "src-tauri/Cargo.toml");
const HF_ORG = "handy-computer";
const CRATE = "transcribe-cpp";

/**
 * Repos in the org that are deliberately absent from the catalog, so the org
 * drift check does not cry wolf every week. Upstream's `gen_catalog.py` hides
 * models whose architecture the pinned transcribe-cpp cannot load yet — offering
 * them would mean shipping a download the app fails to open.
 *
 * A noisy alert is a disabled alert, so keep this list current.
 */
const KNOWN_EXCLUSIONS = [
  "moss-transcribe-diarize",
  "diar_streaming_sortformer_4spk-v2.1",
];

/**
 * How long a model may sit in the org before its absence from the catalog
 * counts as a stall rather than ordinary lag.
 *
 * Upstream regenerates catalog.json in batches, usually alongside a
 * transcribe.cpp bump; the gaps between regenerations run to three weeks
 * (2026-07-28 -> 2026-08-19 is one). Firing the moment a repo appears
 * therefore reports upstream's normal cadence as a fault — and this fork
 * cannot act on it anyway, because regenerating the catalog here means
 * diverging on the one file the daily sync touches most. That is the same
 * trade the crate-pin check below already refused, for the same reason.
 *
 * Thirty days sits clear of the observed cadence, so a `warn` here means
 * upstream has genuinely stopped — the stall this check exists to catch.
 * Inside the window the finding is still printed, just as `info`, so the
 * drift is visible from the first week without costing a red X.
 */
const ORG_DRIFT_GRACE_DAYS = 30;

export interface Finding {
  check: string;
  severity: "info" | "warn";
  message: string;
}

const EXIT_CLEAN = 0;
const EXIT_ERROR = 1;
const EXIT_DRIFT = 2;

/**
 * Whether anything here is worth a human's Monday morning.
 *
 * `info` findings are the check reporting its own limits — no release tag yet,
 * upstream unreachable, a model still inside the grace period. They belong in
 * the report; they do not belong in the job status.
 */
export function hasActionableFinding(findings: Finding[]): boolean {
  return findings.some((f) => f.severity === "warn");
}

/**
 * The process exit code for a finished run.
 *
 * `incomplete` outranks `actionable` deliberately. A sub-check that threw —
 * HuggingFace down, crates.io rate-limiting — leaves us without a full
 * picture, and saying "drift detected" then would repeat the exact fault this
 * script's exit codes were introduced to remove: announcing a failure under
 * the wrong name. We did not find drift; we failed to look.
 */
export function exitCodeFor(state: {
  incomplete: boolean;
  actionable: boolean;
}): number {
  if (state.incomplete) return EXIT_ERROR;
  return state.actionable ? EXIT_DRIFT : EXIT_CLEAN;
}

const slugOf = (repoId: string): string =>
  repoId
    .split("/")
    .pop()!
    .replace(/-gguf$/, "");

/** Catalog model ids, e.g. `handy-computer/whisper-small-gguf`. */
export function catalogRepoIds(catalogJson: string): string[] {
  const parsed = JSON.parse(catalogJson) as { models?: { id?: string }[] };
  return (parsed.models ?? [])
    .map((m) => m.id)
    .filter((id): id is string => typeof id === "string");
}

/** The `transcribe-cpp = { version = "x.y.z"` pin from Cargo.toml. */
export function pinnedCrateVersion(
  cargoToml: string,
  crate: string,
): string | null {
  const re = new RegExp(
    `^\\s*${crate}\\s*=\\s*\\{[^}]*?version\\s*=\\s*"([^"]+)"`,
    "m",
  );
  return cargoToml.match(re)?.[1] ?? null;
}

/** Compare dotted numeric versions. Returns >0 when `a` is newer than `b`. */
export function compareVersions(a: string, b: string): number {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

function git(cmd: string): string | null {
  try {
    return execSync(`git ${cmd}`, {
      cwd: REPO_ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return null;
  }
}

async function fetchJson(url: string): Promise<unknown> {
  const res = await fetch(url, {
    headers: { "user-agent": "handy-plus-model-currency-check" },
  });
  if (!res.ok) throw new Error(`${url} -> HTTP ${res.status}`);
  return res.json();
}

// ── check 1: is our catalog behind upstream's? ────────────────────────────────
function checkCatalogDrift(): Finding[] {
  // `--no-tags`: this fork's release tags are the only ones that should be
  // reachable here. Upstream's whole tag history otherwise lands in the local
  // repo, and `git describe` in checkShippingDrift below picks the *nearest*
  // reachable tag — so on any run where main has advanced past this fork's
  // last release without a new tag (a sync that merged but skipped the
  // release), an upstream tag can win and the catalog gets diffed against a
  // release that was never ours. Nothing here needs upstream's tags.
  git("fetch upstream --no-tags --quiet");
  const diff = git("diff --stat HEAD upstream/main -- src-tauri/src/catalog/");
  if (diff === null) {
    return [
      {
        check: "catalog-drift",
        severity: "info",
        message:
          "Could not compare against upstream/main (no upstream remote or fetch failed).",
      },
    ];
  }
  if (diff === "") return [];
  return [
    {
      check: "catalog-drift",
      severity: "warn",
      message:
        "Our catalog differs from upstream/main. The daily sync may be stuck — " +
        `check for an open sync PR.\n${diff}`,
    },
  ];
}

// ── check 2: models published by the org but absent from the catalog ──────────

/** A repo in the org with no catalog entry. `ageDays` is null if unknowable. */
export interface MissingModel {
  slug: string;
  ageDays: number | null;
}

/**
 * Split the missing repos by age against the grace period. Pure, so the
 * boundary is testable without reaching for the network.
 *
 * A repo whose age could not be read never escalates on its own. The
 * alternative — treating "unknown" as "old" — turns one flaky HuggingFace
 * response into a red X on a watchdog, which is how a watchdog gets muted.
 * Erring the other way costs at most a week: the next Monday run re-reads it.
 */
export function orgDriftFindings(
  missing: MissingModel[],
  graceDays: number,
): Finding[] {
  if (missing.length === 0) return [];

  const listed = missing
    .map(
      (m) =>
        `  ${m.slug}` +
        (m.ageDays === null
          ? " (publication date unavailable)"
          : ` (published ${m.ageDays}d ago)`),
    )
    .join("\n");

  const stalled = missing.filter(
    (m) => m.ageDays !== null && m.ageDays >= graceDays,
  );

  if (stalled.length === 0) {
    return [
      {
        check: "org-drift",
        severity: "info",
        message:
          `${missing.length} model(s) published by ${HF_ORG} are not in our ` +
          `catalog, none of them older than ${graceDays} days. Upstream ` +
          "regenerates the catalog in batches, so this is ordinary lag until " +
          "one of them ages out:\n" +
          listed,
      },
    ];
  }

  return [
    {
      check: "org-drift",
      severity: "warn",
      message:
        `${stalled.length} model(s) published by ${HF_ORG} have been missing ` +
        `from our catalog for over ${graceDays} days. Upstream has most likely ` +
        "stopped regenerating it (or KNOWN_EXCLUSIONS needs updating):\n" +
        listed,
    },
  ];
}

/** Days since a repo was created on the Hub, or null if that cannot be read. */
async function repoAgeDays(
  repoId: string,
  now: number,
): Promise<number | null> {
  try {
    const meta = (await fetchJson(
      `https://huggingface.co/api/models/${repoId}`,
    )) as { createdAt?: string };
    const created = Date.parse(meta.createdAt ?? "");
    if (Number.isNaN(created)) return null;
    return Math.floor((now - created) / 86_400_000);
  } catch {
    return null;
  }
}

async function checkOrgDrift(catalogIds: string[]): Promise<Finding[]> {
  const models = (await fetchJson(
    `https://huggingface.co/api/models?author=${HF_ORG}&limit=1000`,
  )) as { id?: string }[];

  const known = new Set(catalogIds.map(slugOf));
  const missingIds = models
    .map((m) => m.id)
    .filter((id): id is string => typeof id === "string")
    .filter((id) => id.endsWith("-gguf"))
    .filter(
      (id) => !known.has(slugOf(id)) && !KNOWN_EXCLUSIONS.includes(slugOf(id)),
    );

  if (missingIds.length === 0) return [];

  // Only the missing repos are looked up individually — normally none, and a
  // handful at worst. The org listing does not carry creation dates.
  const now = Date.now();
  const missing = await Promise.all(
    missingIds.map(async (id) => ({
      slug: slugOf(id),
      ageDays: await repoAgeDays(id, now),
    })),
  );

  return orgDriftFindings(missing, ORG_DRIFT_GRACE_DAYS);
}

// ── check 3: is the transcribe-cpp pin behind the one upstream chose? ────────
async function checkCratePin(cargoToml: string): Promise<Finding[]> {
  const pinned = pinnedCrateVersion(cargoToml, CRATE);
  if (!pinned) {
    return [
      {
        check: "crate-pin",
        severity: "warn",
        message: `Could not find a ${CRATE} version pin in Cargo.toml.`,
      },
    ];
  }

  const meta = (await fetchJson(
    `https://crates.io/api/v1/crates/${CRATE}`,
  )) as {
    crate?: { max_stable_version?: string; max_version?: string };
  };
  const latest = meta.crate?.max_stable_version ?? meta.crate?.max_version;
  if (!latest) throw new Error(`crates.io returned no version for ${CRATE}`);

  // Measured against upstream's pin, not against crates.io alone.
  //
  // This fork does not choose this crate's version — upstream does, and the
  // daily sync carries the choice across. Firing whenever crates.io moves
  // ahead therefore reported a state this fork must not act on: bumping ahead
  // of upstream means diverging on `Cargo.toml`, which is tied for the highest
  // upstream churn of any file this fork has touched, and buying a permanent
  // conflict there to lead upstream by a patch release is a bad trade. This
  // ran red for two straight weeks on exactly that, which is how a signal
  // becomes furniture.
  const upstream = await fetchUpstreamPin();

  // Actually actionable: this fork is behind its own source of truth, so a
  // sync has not landed or a merge kept the wrong side.
  if (upstream && compareVersions(upstream, pinned) > 0) {
    return [
      {
        check: "crate-pin",
        severity: "warn",
        message:
          `${CRATE} is pinned at ${pinned} here but upstream is on ${upstream}. ` +
          "The sync should have carried that across — check for a stalled or " +
          "mis-resolved upstream merge.",
      },
    ];
  }

  // Also actionable even though upstream has not moved: a minor bump is where
  // new model architectures land, and a model can sit in the catalog and still
  // fail to load without the crate that understands it. A patch release cannot
  // add an architecture, so it is not worth a word.
  const series = (v: string) => v.split(".").slice(0, 2).join(".");
  if (
    compareVersions(latest, pinned) > 0 &&
    compareVersions(series(latest), series(pinned)) !== 0
  ) {
    return [
      {
        check: "crate-pin",
        severity: "warn",
        message:
          `${CRATE} is pinned at ${pinned} but ${latest} is published` +
          (upstream ? ` (upstream is still on ${upstream})` : "") +
          ". A minor bump is where new model architectures arrive — a model can " +
          "sit in the catalog and still fail to load without it. Prefer waiting " +
          "for upstream to bump, so this fork does not diverge on Cargo.toml.",
      },
    ];
  }

  return [];
}

/**
 * Upstream's pin for the same crate, read straight from its default branch.
 * Returns null rather than throwing: not knowing what upstream pins is a reason
 * to fall back to the crates.io comparison, not to fail the run.
 */
async function fetchUpstreamPin(): Promise<string | null> {
  try {
    const res = await fetch(
      "https://raw.githubusercontent.com/cjpais/Handy/main/src-tauri/Cargo.toml",
      { headers: { "user-agent": "handy-plus-model-currency-check" } },
    );
    if (!res.ok) return null;
    return pinnedCrateVersion(await res.text(), CRATE);
  } catch {
    return null;
  }
}

// ── check 4: catalog changed since the last release we shipped ────────────────
function checkShippingDrift(): Finding[] {
  const lastTag = git("describe --tags --abbrev=0");
  if (!lastTag) {
    return [
      {
        check: "shipping-drift",
        severity: "info",
        message: "No release tag found; skipping the shipped-vs-merged check.",
      },
    ];
  }

  const diff = git(`diff --stat ${lastTag}..HEAD -- src-tauri/src/catalog/`);
  if (!diff) return [];
  return [
    {
      check: "shipping-drift",
      severity: "warn",
      message:
        `The catalog has changed since ${lastTag}, so those models are merged ` +
        "but not shipped. The catalog is baked into the binary at build time — " +
        "users cannot see them until a release is cut.\n" +
        diff,
    },
  ];
}

async function main(): Promise<void> {
  const catalogJson = readFileSync(CATALOG_PATH, "utf8");
  const cargoToml = readFileSync(CARGO_PATH, "utf8");
  const catalogIds = catalogRepoIds(catalogJson);

  const findings: Finding[] = [...checkCatalogDrift(), ...checkShippingDrift()];

  // Network checks are settled individually so one outage cannot mask the
  // others, and a failure is reported rather than swallowed into "all clear".
  let incomplete = false;
  for (const [name, run] of [
    ["org-drift", () => checkOrgDrift(catalogIds)],
    ["crate-pin", () => checkCratePin(cargoToml)],
  ] as const) {
    try {
      findings.push(...(await run()));
    } catch (error) {
      // Still `warn`, so it reads as needing attention in the report — but it
      // is a failure to look, not a thing seen, and the exit code says so.
      incomplete = true;
      findings.push({
        check: name,
        severity: "warn",
        message: `Check could not run: ${(error as Error).message}`,
      });
    }
  }

  const actionable = hasActionableFinding(findings);

  // `actionable` and `incomplete` are additive: every field the previous shape
  // carried is still here and unchanged, so anything already reading this JSON
  // keeps working.
  const summary = {
    catalogModelCount: catalogIds.length,
    pinnedTranscribeCpp: pinnedCrateVersion(cargoToml, CRATE),
    actionable,
    incomplete,
    findings,
  };

  // Set rather than thrown, so stdout is flushed before the process leaves.
  process.exitCode = exitCodeFor({ incomplete, actionable });

  if (process.argv.includes("--json")) {
    console.log(JSON.stringify(summary, null, 2));
    return;
  }

  console.log(`Catalog models: ${summary.catalogModelCount}`);
  console.log(`transcribe-cpp pin: ${summary.pinnedTranscribeCpp}`);
  if (findings.length === 0) {
    // Wording held stable on purpose: it was a load-bearing string for the old
    // grep-based gate, and costs nothing to keep for anything else reading it.
    console.log("\nNo drift detected — the model stack is current.");
    return;
  }
  console.log(`\n${findings.length} finding(s):\n`);
  for (const f of findings) {
    console.log(`[${f.severity}] ${f.check}: ${f.message}\n`);
  }
  console.log(
    incomplete
      ? "The check could not complete — see the findings above. No conclusion " +
          "about drift should be drawn from this run."
      : actionable
        ? "Actionable drift detected — see the findings above."
        : "No actionable drift — every finding above is informational.",
  );
}

// Only run when invoked directly, so the pure helpers above stay importable
// from a test without firing network calls.
if (import.meta.main !== false) {
  main().catch((error) => {
    console.error(`model-currency check failed: ${(error as Error).message}`);
    process.exit(EXIT_ERROR);
  });
}
