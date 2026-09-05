// Standalone assert check (no JS unit-test runner in this repo). Run with:
//   bun src/i18n/overlay.test.ts
import assert from "node:assert";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  buildLocaleResources,
  mergeOverlay,
  leafPaths,
  shapeClashes,
  valueAt,
  rebrand,
  type TranslationTree,
} from "./overlay";

const LOCALES_DIR = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "locales",
);

// ---------------------------------------------------------------- mergeOverlay

{
  const base: TranslationTree = { a: "1", nested: { keep: "k", drop: "old" } };
  const overlay: TranslationTree = {
    nested: { drop: "new", added: "x" },
    top: "t",
  };
  const merged = mergeOverlay(base, overlay);

  assert.deepStrictEqual(merged, {
    a: "1",
    nested: { keep: "k", drop: "new", added: "x" },
    top: "t",
  });

  // Neither input may be touched. The caller's objects are shared module
  // objects from `import.meta.glob`, so a mutating merge would corrupt every
  // other reader of that locale.
  assert.deepStrictEqual(base, { a: "1", nested: { keep: "k", drop: "old" } });
  assert.deepStrictEqual(overlay, {
    nested: { drop: "new", added: "x" },
    top: "t",
  });
}

// No overlay at all is the common case - 23 of 24 locales - and must be a
// faithful pass-through, not a crash and not a dropped locale.
assert.deepStrictEqual(mergeOverlay({ a: "1" }, undefined), { a: "1" });
assert.deepStrictEqual(mergeOverlay({ a: "1" }, null), { a: "1" });

// Arrays replace rather than merge: an overlay supplying a list means "use
// this list", and element-wise merging of translation arrays has no meaning.
assert.deepStrictEqual(
  mergeOverlay({ list: ["a", "b", "c"] }, { list: ["z"] }),
  { list: ["z"] },
);

// A shape disagreement resolves to the overlay rather than throwing, because
// at runtime something has to render. shapeClashes below is what reports it.
assert.deepStrictEqual(mergeOverlay({ a: { b: "1" } }, { a: "flat" }), {
  a: "flat",
});

// Locale files are parsed JSON, so `__proto__` is reachable from data. It must
// never reach the prototype chain.
{
  const hostile = JSON.parse('{"__proto__": {"polluted": true}, "ok": "yes"}');
  const merged = mergeOverlay({ ok: "no" }, hostile);
  assert.strictEqual(merged.ok, "yes");
  assert.strictEqual(({} as Record<string, unknown>).polluted, undefined);
  assert.strictEqual(Object.getPrototypeOf(merged), Object.prototype);
}

// ------------------------------------------------------ buildLocaleResources
//
// The wiring index.ts uses. Its one real failure mode is silent: if the two
// globs disagreed about how a locale is named, every overlay would attach to
// nothing and the app would render upstream's English with no error anywhere.

{
  const resources = buildLocaleResources(
    {
      "./locales/en/translation.json": { default: { a: "up", b: "keep" } },
      "./locales/de/translation.json": { default: { a: "de-up" } },
      "./locales/zh-TW/translation.json": { default: { a: "tw" } },
    },
    { "./locales/en/plus.json": { default: { a: "fork", c: "added" } } },
  );

  assert.deepStrictEqual(Object.keys(resources).sort(), ["de", "en", "zh-TW"]);
  assert.deepStrictEqual(resources.en, { a: "fork", b: "keep", c: "added" });
  // A locale with no overlay is passed through untouched - 23 of 24 today.
  assert.deepStrictEqual(resources.de, { a: "de-up" });
  // A hyphenated locale directory must survive intact, or zh-TW loses its
  // overlay while looking like it has one.
  assert.deepStrictEqual(resources["zh-TW"], { a: "tw" });
}

// An overlay for a language with no base file contributes nothing rather than
// inventing a half-populated locale in the language picker - and says so,
// because an ignored overlay is otherwise indistinguishable from a correct app.
{
  const warnings: string[] = [];
  const realWarn = console.warn;
  console.warn = (...args: unknown[]) => warnings.push(args.join(" "));
  try {
    assert.deepStrictEqual(
      buildLocaleResources(
        { "./locales/en/translation.json": { default: { a: "1" } } },
        { "./locales/xx/plus.json": { default: { a: "2" } } },
      ),
      { en: { a: "1" } },
    );
  } finally {
    console.warn = realWarn;
  }
  assert.strictEqual(
    warnings.length,
    1,
    "an orphaned overlay must be reported",
  );
  assert.match(warnings[0], /xx\/plus\.json/);
}

// The ordinary case must stay silent - a warning nobody can act on is a
// warning everybody learns to ignore.
{
  const warnings: string[] = [];
  const realWarn = console.warn;
  console.warn = (...args: unknown[]) => warnings.push(args.join(" "));
  try {
    buildLocaleResources(
      {
        "./locales/en/translation.json": { default: { a: "1" } },
        "./locales/de/translation.json": { default: { a: "2" } },
      },
      { "./locales/en/plus.json": { default: { a: "fork" } } },
    );
  } finally {
    console.warn = realWarn;
  }
  assert.deepStrictEqual(warnings, []);
}

// --------------------------------------------------------------- shapeClashes

// An ordinary override and an ordinary addition are not clashes.
assert.deepStrictEqual(
  shapeClashes(
    { a: "up", g: { x: "1" } },
    { a: "fork", g: { y: "2" }, n: "new" },
  ),
  [],
);

// An overlay group where upstream has a string. This is the dangerous one: leaf
// by leaf it looks like a plain addition, and merging it deletes upstream's
// string with nothing reporting it.
assert.deepStrictEqual(shapeClashes({ a: "up" }, { a: { b: "fork" } }), [
  ["a", "b"],
]);

// An overlay string where upstream has a group - the whole group would vanish.
assert.deepStrictEqual(shapeClashes({ a: { b: "up" } }, { a: "fork" }), [
  ["a"],
]);

// ...and the merge really does destroy it, which is why the check exists.
assert.deepStrictEqual(mergeOverlay({ a: "up" }, { a: { b: "fork" } }), {
  a: { b: "fork" },
});

// -------------------------------------------------------------------- rebrand

assert.strictEqual(
  rebrand("Restart Handy to apply"),
  "Restart Handy Plus to apply",
);
assert.strictEqual(rebrand("Handy's models"), "Handy Plus's models");
// Idempotent: re-running over an already-rebranded string must not compound.
assert.strictEqual(rebrand(rebrand("About Handy")), "About Handy Plus");
assert.strictEqual(rebrand("Handy Plus"), "Handy Plus");
// Whole words only - never a substring of a longer identifier.
assert.strictEqual(rebrand("HandyCam"), "HandyCam");

// ------------------------------------------------- the real locale files
//
// The invariant that makes this fork's locale files mergeable: every string
// the fork overrides is *only* a rebrand of the upstream string it replaces.
// If upstream rewords one of those strings, the override stops matching and
// this fails - which is the signal that the fork is now shipping stale English
// rather than a rebrand. Additions (keys upstream does not have) are exempt;
// they have no upstream text to drift from.

const locales = fs
  .readdirSync(LOCALES_DIR, { withFileTypes: true })
  .filter((e) => e.isDirectory())
  .map((e) => e.name);

let overrideCount = 0;
let overlayCount = 0;

for (const lang of locales) {
  const overlayPath = path.join(LOCALES_DIR, lang, "plus.json");
  if (!fs.existsSync(overlayPath)) continue;
  overlayCount++;

  const base = JSON.parse(
    fs.readFileSync(path.join(LOCALES_DIR, lang, "translation.json"), "utf8"),
  ) as TranslationTree;
  const overlay = JSON.parse(
    fs.readFileSync(overlayPath, "utf8"),
  ) as TranslationTree;

  assert.deepStrictEqual(
    shapeClashes(base, overlay).map((p) => p.join(".")),
    [],
    `${lang}/plus.json puts a group where upstream has a string, or the ` +
      `reverse. Merging that deletes upstream copy silently.`,
  );

  for (const p of leafPaths(overlay)) {
    const upstream = valueAt(base, p);
    if (upstream === undefined) continue; // an addition, not an override
    overrideCount++;
    assert.strictEqual(
      typeof upstream,
      "string",
      `${lang}/plus.json overrides ${p.join(".")}, which is not a string upstream`,
    );
    assert.strictEqual(
      valueAt(overlay, p),
      rebrand(upstream as string),
      `${lang}/plus.json: "${p.join(".")}" is no longer a plain rebrand of the ` +
        `upstream string. Upstream now says ${JSON.stringify(upstream)}; either ` +
        `re-derive the override from it or record why it deliberately differs.`,
    );
  }

  // The merge must not lose an upstream key, and must not leave a stale
  // un-rebranded product name behind on a path the overlay claims to own.
  const merged = mergeOverlay(base, overlay);
  for (const p of leafPaths(base)) {
    assert.notStrictEqual(
      valueAt(merged, p),
      undefined,
      `${lang}: merging plus.json dropped upstream key ${p.join(".")}`,
    );
  }
}

assert.ok(
  overlayCount > 0,
  "no plus.json overlay found - is the split still in place?",
);
assert.ok(
  overrideCount > 0,
  "no overrides found - the rebrand audit is checking nothing",
);

console.log(
  `i18n overlay: all assertions passed (${overlayCount} overlay file(s), ${overrideCount} overrides audited)`,
);
