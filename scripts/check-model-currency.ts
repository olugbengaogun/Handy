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
 * Exits 0 always in report mode; the workflow decides what to do with the JSON
 * summary on stdout. Exit 1 only on an internal error, so a transient network
 * failure never silently reports "all clear".
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

export interface Finding {
  check: string;
  severity: "info" | "warn";
  message: string;
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
  git("fetch upstream --quiet");
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
async function checkOrgDrift(catalogIds: string[]): Promise<Finding[]> {
  const models = (await fetchJson(
    `https://huggingface.co/api/models?author=${HF_ORG}&limit=1000`,
  )) as { id?: string }[];

  const known = new Set(catalogIds.map(slugOf));
  const missing = models
    .map((m) => m.id)
    .filter((id): id is string => typeof id === "string")
    .filter((id) => id.endsWith("-gguf"))
    .map(slugOf)
    .filter((slug) => !known.has(slug) && !KNOWN_EXCLUSIONS.includes(slug));

  if (missing.length === 0) return [];
  return [
    {
      check: "org-drift",
      severity: "warn",
      message:
        `${missing.length} model(s) published by ${HF_ORG} are not in our catalog. ` +
        "This is the earliest signal that upstream has stopped regenerating it " +
        `(or that KNOWN_EXCLUSIONS needs updating):\n  ${missing.join("\n  ")}`,
    },
  ];
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
  for (const [name, run] of [
    ["org-drift", () => checkOrgDrift(catalogIds)],
    ["crate-pin", () => checkCratePin(cargoToml)],
  ] as const) {
    try {
      findings.push(...(await run()));
    } catch (error) {
      findings.push({
        check: name,
        severity: "warn",
        message: `Check could not run: ${(error as Error).message}`,
      });
    }
  }

  const summary = {
    catalogModelCount: catalogIds.length,
    pinnedTranscribeCpp: pinnedCrateVersion(cargoToml, CRATE),
    findings,
  };

  if (process.argv.includes("--json")) {
    console.log(JSON.stringify(summary, null, 2));
    return;
  }

  console.log(`Catalog models: ${summary.catalogModelCount}`);
  console.log(`transcribe-cpp pin: ${summary.pinnedTranscribeCpp}`);
  if (findings.length === 0) {
    console.log("\nNo drift detected — the model stack is current.");
    return;
  }
  console.log(`\n${findings.length} finding(s):\n`);
  for (const f of findings) {
    console.log(`[${f.severity}] ${f.check}: ${f.message}\n`);
  }
}

// Only run when invoked directly, so the pure helpers above stay importable
// from a test without firing network calls.
if (import.meta.main !== false) {
  main().catch((error) => {
    console.error(`model-currency check failed: ${(error as Error).message}`);
    process.exit(1);
  });
}
