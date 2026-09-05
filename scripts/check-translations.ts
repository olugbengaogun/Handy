import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import {
  leafPaths,
  rebrand,
  valueAt,
  type TranslationTree,
} from "../src/i18n/overlay";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Configuration
const LOCALES_DIR = path.join(__dirname, "..", "src", "i18n", "locales");
const REFERENCE_LANG = "en";

type TranslationData = Record<string, unknown>;

interface ValidationResult {
  valid: boolean;
  missing: string[][];
  extra: string[][];
}

function getAllLanguages(): string[] {
  const entries = fs.readdirSync(LOCALES_DIR, { withFileTypes: true });
  return entries
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
}

function getLanguages(): string[] {
  return getAllLanguages().filter((lang) => lang !== REFERENCE_LANG);
}

const LANGUAGES = getLanguages();

// Colors for terminal output
const colors: Record<string, string> = {
  reset: "\x1b[0m",
  red: "\x1b[31m",
  green: "\x1b[32m",
  yellow: "\x1b[33m",
  blue: "\x1b[34m",
};

function colorize(text: string, color: string): string {
  return `${colors[color]}${text}${colors.reset}`;
}

function getAllKeyPaths(
  obj: TranslationData,
  prefix: string[] = [],
): string[][] {
  let paths: string[][] = [];
  for (const key in obj) {
    if (!Object.hasOwn(obj, key)) continue;

    const currentPath = prefix.concat([key]);
    const value = obj[key];

    if (typeof value === "object" && value !== null && !Array.isArray(value)) {
      paths = paths.concat(
        getAllKeyPaths(value as TranslationData, currentPath),
      );
    } else {
      paths.push(currentPath);
    }
  }
  return paths;
}

function hasKeyPath(obj: TranslationData, keyPath: string[]): boolean {
  let current: unknown = obj;
  for (const key of keyPath) {
    if (
      typeof current !== "object" ||
      current === null ||
      (current as Record<string, unknown>)[key] === undefined
    ) {
      return false;
    }
    current = (current as Record<string, unknown>)[key];
  }
  return true;
}

function loadTranslationFile(lang: string): TranslationData | null {
  const filePath = path.join(LOCALES_DIR, lang, "translation.json");

  try {
    const content = fs.readFileSync(filePath, "utf8");
    return JSON.parse(content) as TranslationData;
  } catch (error) {
    console.error(colorize(`✗ Error loading ${lang}/translation.json:`, "red"));
    console.error(`  ${(error as Error).message}`);
    return null;
  }
}

function loadOverlayFile(lang: string): TranslationData | null {
  const filePath = path.join(LOCALES_DIR, lang, "plus.json");
  if (!fs.existsSync(filePath)) return null;
  try {
    return JSON.parse(fs.readFileSync(filePath, "utf8")) as TranslationData;
  } catch (error) {
    console.error(colorize(`✗ Error loading ${lang}/plus.json:`, "red"));
    console.error(`  ${(error as Error).message}`);
    return null;
  }
}

/**
 * Audit the Handy Plus overlays (see src/i18n/overlay.ts).
 *
 * `translation.json` is upstream's file, byte for byte; `plus.json` carries
 * everything this fork changes. The comparison above therefore measures what
 * it is supposed to measure again - how complete the *translations* are -
 * instead of reporting this fork's untranslated English additions as 122
 * missing keys in all 23 languages, which is what made it fail permanently and
 * why it is `continue-on-error` in code-quality.yml.
 *
 * What is checked here is the one thing the split can silently get wrong: an
 * override that has stopped matching the upstream string it rebrands. In-file
 * rebranding surfaced an upstream reword as a merge conflict; an overlay does
 * not, so it is surfaced here instead.
 *
 * Returns true if every override still lines up.
 */
function auditOverlays(): boolean {
  const overlaid = getAllLanguages().filter((lang) =>
    fs.existsSync(path.join(LOCALES_DIR, lang, "plus.json")),
  );
  if (overlaid.length === 0) return true;

  console.log(colorize("\nHandy Plus overlay (plus.json):", "blue"));
  console.log("─".repeat(60));

  let ok = true;
  for (const lang of overlaid) {
    const base = loadTranslationFile(lang);
    const overlay = loadOverlayFile(lang);
    if (!base || !overlay) {
      ok = false;
      continue;
    }

    // Held as path arrays, never as joined strings: four real keys contain a
    // dot (`parakeet-tdt-0.6b-v2`), so splitting a joined path back apart to
    // look a value up would report `undefined` for exactly the keys most in
    // need of a readable diagnostic.
    const drifted: string[][] = [];
    const shapeClash: string[][] = [];
    let overrides = 0;
    let additions = 0;

    for (const keyPath of leafPaths(overlay as TranslationTree)) {
      const upstream = valueAt(base as TranslationTree, keyPath);
      if (upstream === undefined) {
        additions++;
        continue;
      }
      overrides++;
      if (typeof upstream !== "string") {
        shapeClash.push(keyPath);
      } else if (
        valueAt(overlay as TranslationTree, keyPath) !== rebrand(upstream)
      ) {
        drifted.push(keyPath);
      }
    }

    console.log(
      `${lang.toUpperCase()}: ${overrides} override(s), ${additions} fork-only key(s)`,
    );

    for (const keyPath of shapeClash) {
      ok = false;
      console.log(
        colorize(
          `  ✗ ${keyPath.join(".")}: overrides a group, not a string`,
          "red",
        ),
      );
    }
    for (const keyPath of drifted) {
      ok = false;
      console.log(
        colorize(
          `  ✗ ${keyPath.join(".")}: no longer a plain rebrand of upstream`,
          "red",
        ),
      );
      console.log(
        `      upstream: ${JSON.stringify(valueAt(base as TranslationTree, keyPath))}`,
      );
      console.log(
        `      ours:     ${JSON.stringify(valueAt(overlay as TranslationTree, keyPath))}`,
      );
    }
    if (drifted.length === 0 && shapeClash.length === 0) {
      console.log(colorize("  ✓ every override is a plain rebrand", "green"));
    }
  }

  if (!ok) {
    console.log(
      colorize(
        "\n  Upstream reworded a string this fork overrides. Re-derive the",
        "yellow",
      ),
    );
    console.log(
      colorize(
        "  override from the new upstream text, or record why it differs.",
        "yellow",
      ),
    );
  }
  return ok;
}

function validateTranslations(): void {
  console.log(colorize("\n🌍 Translation Consistency Check\n", "blue"));

  // Load reference file
  console.log(`Loading reference language: ${REFERENCE_LANG}`);
  const referenceData = loadTranslationFile(REFERENCE_LANG);

  if (!referenceData) {
    console.error(
      colorize(`\n✗ Failed to load reference file (${REFERENCE_LANG})`, "red"),
    );
    process.exit(1);
  }

  // Get all key paths from reference
  const referenceKeyPaths = getAllKeyPaths(referenceData);
  console.log(`Reference has ${referenceKeyPaths.length} keys\n`);

  // Track validation results
  let hasErrors = false;
  const results: Record<string, ValidationResult> = {};

  // Validate each language
  for (const lang of LANGUAGES) {
    const langData = loadTranslationFile(lang);

    if (!langData) {
      hasErrors = true;
      results[lang] = { valid: false, missing: [], extra: [] };
      continue;
    }

    // Find missing keys
    const missing = referenceKeyPaths.filter(
      (keyPath) => !hasKeyPath(langData, keyPath),
    );

    // Find extra keys (keys in language but not in reference)
    const langKeyPaths = getAllKeyPaths(langData);
    const extra = langKeyPaths.filter(
      (keyPath) => !hasKeyPath(referenceData, keyPath),
    );

    results[lang] = {
      valid: missing.length === 0 && extra.length === 0,
      missing,
      extra,
    };

    if (missing.length > 0 || extra.length > 0) {
      hasErrors = true;
    }
  }

  // Print results
  console.log(colorize("Results:", "blue"));
  console.log("─".repeat(60));

  for (const lang of LANGUAGES) {
    const result = results[lang];

    if (result.valid) {
      console.log(
        colorize(`✓ ${lang.toUpperCase()}: All keys present`, "green"),
      );
    } else {
      console.log(colorize(`✗ ${lang.toUpperCase()}: Issues found`, "red"));

      if (result.missing.length > 0) {
        console.log(
          colorize(`  Missing ${result.missing.length} keys:`, "yellow"),
        );
        result.missing.slice(0, 10).forEach((keyPath) => {
          console.log(`    - ${keyPath.join(".")}`);
        });
        if (result.missing.length > 10) {
          console.log(
            colorize(
              `    ... and ${result.missing.length - 10} more`,
              "yellow",
            ),
          );
        }
      }

      if (result.extra.length > 0) {
        console.log(
          colorize(
            `  Extra ${result.extra.length} keys (not in reference):`,
            "yellow",
          ),
        );
        result.extra.slice(0, 10).forEach((keyPath) => {
          console.log(`    - ${keyPath.join(".")}`);
        });
        if (result.extra.length > 10) {
          console.log(
            colorize(`    ... and ${result.extra.length - 10} more`, "yellow"),
          );
        }
      }

      console.log("");
    }
  }

  console.log("─".repeat(60));

  // Runs unconditionally, and its verdict is folded into the exit code: a
  // drifted override is a real defect (stale English shipped to users) and
  // must not be hidden behind an unrelated language being incomplete.
  if (!auditOverlays()) {
    hasErrors = true;
  }

  // Summary
  const validCount = Object.values(results).filter((r) => r.valid).length;
  const totalCount = LANGUAGES.length;

  if (hasErrors) {
    console.log(
      colorize(
        `\n✗ Validation failed: ${validCount}/${totalCount} languages passed`,
        "red",
      ),
    );
    process.exit(1);
  } else {
    console.log(
      colorize(
        `\n✓ All ${totalCount} languages have complete translations!`,
        "green",
      ),
    );
    process.exit(0);
  }
}

// Run validation
validateTranslations();
