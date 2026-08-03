/**
 * Personal accuracy benchmark for Handy Plus.
 *
 * Every accuracy change in ACCURACY_PLAN.md is a hypothesis. Without a way to
 * measure, "this feels better" is the only available verdict, and that is how
 * accuracy work ships placebo. This harness turns the owner's own dictation into
 * a held-out evaluation set.
 *
 * ## Where the ground truth comes from
 *
 * No labelling is required, because the app already collects both halves:
 *
 *   - **Reference** — `transcription_history.transcription_text`. After the user
 *     hand-corrects a transcript in History, the stored text *is* the corrected
 *     text. That is a human-verified label, produced as a side effect of normal
 *     use.
 *   - **Hypothesis** — a fresh run of the model over the saved WAV, via
 *     `handy --transcribe-file`, which uses the same batch path as the app.
 *
 * So the only prerequisite is that audio retention was on while the samples were
 * collected. Turn `keep_audio_recordings` on for a couple of weeks, correct
 * transcripts as you normally would, then turn it back off — the benchmark set
 * persists in the database.
 *
 * ## Metrics
 *
 *   - **WER** — word error rate. The standard ASR number.
 *   - **CER** — character error rate. Less sensitive to tokenisation, and it
 *     notices casing/punctuation changes that WER's word matching can hide.
 *   - **Edit rate** — the share of transcripts that needed *any* change. This is
 *     the one that actually maps to "do I have to reach for the keyboard", which
 *     is the goal the whole plan is written against. A model can have a decent
 *     WER and still be infuriating if the errors are spread across every
 *     utterance.
 *
 * Usage:
 *   bun run scripts/wer-bench.ts --db <path/to/history.db> [options]
 *
 * Options:
 *   --db <path>       history.db location (required)
 *   --binary <path>   handy executable (default: src-tauri/target/release/handy)
 *   --model <id>      model to evaluate (default: whatever is selected)
 *   --limit <n>       cap the number of samples (default: 200)
 *   --baseline <file> compare against a previous run's JSON and print the delta
 *   --out <file>      write results as JSON for later comparison
 */

import { Database } from "bun:sqlite";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

// ── metrics ───────────────────────────────────────────────────────────────────

/**
 * Normalise before comparing.
 *
 * Lowercased, punctuation stripped, whitespace collapsed. Casing and punctuation
 * are the LLM cleanup layer's job, not the acoustic model's, and leaving them in
 * would make WER move for reasons unrelated to what is being measured. CER is
 * reported separately and does keep them.
 */
export function normalizeForWer(text: string): string[] {
  return text
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s']/gu, " ")
    .split(/\s+/)
    .filter(Boolean);
}

/** Levenshtein distance over any two sequences. */
export function editDistance<T>(a: T[], b: T[]): number {
  if (a.length === 0) return b.length;
  if (b.length === 0) return a.length;

  let prev = Array.from({ length: b.length + 1 }, (_, i) => i);
  let cur = new Array<number>(b.length + 1);

  for (let i = 1; i <= a.length; i++) {
    cur[0] = i;
    for (let j = 1; j <= b.length; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      cur[j] = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost);
    }
    [prev, cur] = [cur, prev];
  }
  return prev[b.length];
}

export interface SampleResult {
  id: number;
  words: number;
  wordErrors: number;
  chars: number;
  charErrors: number;
  exact: boolean;
}

export function scoreSample(
  id: number,
  reference: string,
  hypothesis: string,
): SampleResult {
  const refWords = normalizeForWer(reference);
  const hypWords = normalizeForWer(hypothesis);
  const refChars = [...reference.trim()];
  const hypChars = [...hypothesis.trim()];

  return {
    id,
    words: refWords.length,
    wordErrors: editDistance(refWords, hypWords),
    chars: refChars.length,
    charErrors: editDistance(refChars, hypChars),
    // "Exact" uses the normalised form: a transcript differing only in
    // punctuation did not send the user to the keyboard for a *transcription*
    // reason, and the edit rate is meant to track that experience.
    exact: refWords.join(" ") === hypWords.join(" "),
  };
}

export interface Summary {
  samples: number;
  wer: number;
  cer: number;
  editRate: number;
  totalWords: number;
}

export function summarize(results: SampleResult[]): Summary {
  const totalWords = results.reduce((n, r) => n + r.words, 0);
  const totalWordErrors = results.reduce((n, r) => n + r.wordErrors, 0);
  const totalChars = results.reduce((n, r) => n + r.chars, 0);
  const totalCharErrors = results.reduce((n, r) => n + r.charErrors, 0);
  const changed = results.filter((r) => !r.exact).length;

  return {
    samples: results.length,
    // Guarded against an empty set so a run with no usable samples reports 0
    // rather than NaN, which would silently poison a baseline comparison.
    wer: totalWords > 0 ? totalWordErrors / totalWords : 0,
    cer: totalChars > 0 ? totalCharErrors / totalChars : 0,
    editRate: results.length > 0 ? changed / results.length : 0,
    totalWords,
  };
}

// ── harness ───────────────────────────────────────────────────────────────────

interface Args {
  db?: string;
  binary: string;
  model?: string;
  limit: number;
  baseline?: string;
  out?: string;
}

function parseArgs(argv: string[]): Args {
  const get = (flag: string): string | undefined => {
    const i = argv.indexOf(flag);
    return i >= 0 && i + 1 < argv.length ? argv[i + 1] : undefined;
  };

  return {
    db: get("--db"),
    binary:
      get("--binary") ?? join(REPO_ROOT, "src-tauri/target/release/handy"),
    model: get("--model"),
    limit: Number(get("--limit") ?? 200),
    baseline: get("--baseline"),
    out: get("--out"),
  };
}

interface Sample {
  id: number;
  reference: string;
  wavPath: string;
}

/**
 * Pull evaluable samples: entries that still have their audio on disk.
 *
 * Ordered newest-first so a `--limit` run evaluates recent speech, which is what
 * the current dictionary and settings were tuned against.
 */
function loadSamples(dbPath: string, limit: number): Sample[] {
  const db = new Database(dbPath, { readonly: true });
  const recordingsDir = join(dirname(dbPath), "recordings");

  const rows = db
    .query(
      `SELECT id, file_name, transcription_text
       FROM transcription_history
       WHERE has_audio = 1 AND TRIM(transcription_text) != ''
       ORDER BY timestamp DESC
       LIMIT ?`,
    )
    .all(limit) as {
    id: number;
    file_name: string;
    transcription_text: string;
  }[];

  db.close();

  return rows
    .map((row) => ({
      id: row.id,
      reference: row.transcription_text,
      wavPath: join(recordingsDir, row.file_name),
    }))
    .filter((sample) => existsSync(sample.wavPath));
}

/** Transcribe one file with the real binary, returning the raw text. */
function transcribe(
  binary: string,
  wavPath: string,
  model?: string,
): string | null {
  const args = ["--transcribe-file", wavPath];
  if (model) args.push("--model", model);

  const proc = spawnSync(binary, args, { encoding: "utf8", timeout: 300_000 });
  if (proc.status !== 0) {
    console.error(
      `  ! transcription failed for ${wavPath}: ${proc.stderr?.trim() ?? proc.error}`,
    );
    return null;
  }
  return proc.stdout.trim();
}

const pct = (value: number): string => `${(value * 100).toFixed(2)}%`;

function main(): void {
  const args = parseArgs(process.argv.slice(2));

  if (!args.db) {
    console.error(
      "--db is required. Find it under the app data directory shown in Settings → Advanced.",
    );
    process.exit(2);
  }
  if (!existsSync(args.db)) {
    console.error(`No database at ${args.db}`);
    process.exit(2);
  }
  if (!existsSync(args.binary)) {
    console.error(
      `No handy binary at ${args.binary}.\nBuild one with: cd src-tauri && cargo build --release --bin handy`,
    );
    process.exit(2);
  }

  const samples = loadSamples(args.db, args.limit);
  if (samples.length === 0) {
    console.error(
      "No samples with audio on disk.\n" +
        "Turn on Settings → Advanced → Keep audio recordings, dictate for a couple of weeks,\n" +
        "correct transcripts as usual, then re-run this.",
    );
    process.exit(1);
  }

  console.log(`Evaluating ${samples.length} sample(s)...\n`);

  const results: SampleResult[] = [];
  for (const [index, sample] of samples.entries()) {
    const hypothesis = transcribe(args.binary, sample.wavPath, args.model);
    if (hypothesis === null) continue;

    const result = scoreSample(sample.id, sample.reference, hypothesis);
    results.push(result);

    const wer = result.words > 0 ? result.wordErrors / result.words : 0;
    process.stdout.write(
      `  [${index + 1}/${samples.length}] #${sample.id} WER ${pct(wer)}\r`,
    );
  }
  process.stdout.write("\n\n");

  const summary = summarize(results);
  console.log(`Samples:    ${summary.samples}`);
  console.log(`Words:      ${summary.totalWords}`);
  console.log(`WER:        ${pct(summary.wer)}`);
  console.log(`CER:        ${pct(summary.cer)}`);
  console.log(`Edit rate:  ${pct(summary.editRate)}`);

  if (args.baseline && existsSync(args.baseline)) {
    const before = JSON.parse(readFileSync(args.baseline, "utf8")) as Summary;
    // Negative deltas are improvements for all three metrics.
    const delta = (now: number, then: number) => {
      const d = now - then;
      const sign = d > 0 ? "+" : "";
      const verdict = d < 0 ? "better" : d > 0 ? "WORSE" : "same";
      return `${sign}${(d * 100).toFixed(2)}pp (${verdict})`;
    };
    console.log("\nversus baseline:");
    console.log(`  WER:       ${delta(summary.wer, before.wer)}`);
    console.log(`  CER:       ${delta(summary.cer, before.cer)}`);
    console.log(`  Edit rate: ${delta(summary.editRate, before.editRate)}`);
  }

  if (args.out) {
    writeFileSync(args.out, `${JSON.stringify(summary, null, 2)}\n`);
    console.log(`\nWrote ${args.out}`);
  }
}

// Only run when invoked directly, so the metric helpers stay importable from a
// test without touching the database or spawning anything.
if (import.meta.main !== false) {
  main();
}
