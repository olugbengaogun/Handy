/**
 * The Handy Plus translation overlay.
 *
 * `locales/<lang>/translation.json` is upstream's file and is kept **byte
 * identical to cjpais/Handy**. Everything this fork changes about the English
 * copy - the rebrand, and the strings for features upstream does not have -
 * lives beside it in `locales/<lang>/plus.json` and is merged in here at load
 * time.
 *
 * That split is not tidiness. Editing upstream's file in place is what made
 * `en/translation.json` the single most conflict-prone path in the repository:
 * upstream touches it in most commits, and a fork edit on the same line turns
 * every one of those into a merge conflict that stops the daily sync dead. On
 * 2026-08-31 exactly that collision (one reworded string) blocked six upstream
 * commits for five days. With the fork's edits moved out, that file merges
 * fast-forward forever, and the only thing a human ever has to look at is a
 * real disagreement about behaviour.
 *
 * The reverse risk - upstream rewording a string this fork overrides, and the
 * override silently keeping the old text - is covered by the audit in
 * `scripts/check-translations.ts`, which re-derives every override from the
 * current upstream string and reports any that no longer match.
 */

export type TranslationNode =
  | string
  | number
  | boolean
  | null
  | TranslationTree;

export interface TranslationTree {
  [key: string]: TranslationNode | TranslationNode[];
}

/** `Object.hasOwn` in spirit; spelled out because this file targets ES2020. */
function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

/**
 * A plain, non-array object - the only shape that is merged rather than
 * replaced. Arrays are deliberately excluded: an overlay that supplies a list
 * means "use this list", and element-wise merging of translation arrays has no
 * defensible semantics.
 */
function isMergeable(value: unknown): value is TranslationTree {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    // Anything exotic (Date, Map, a prototype-carrying object) is not
    // something a JSON locale file can produce, so treating it as a leaf is
    // both correct and the safe direction to be wrong in.
    (Object.getPrototypeOf(value) === Object.prototype ||
      Object.getPrototypeOf(value) === null)
  );
}

/**
 * `base` with `overlay` applied on top. Neither input is mutated: every node
 * the overlay touches is rebuilt, and untouched subtrees are shared by
 * reference. That matters because the caller's objects come from
 * `import.meta.glob`, which hands the *same* module object to every importer -
 * merging in place would corrupt every other reader of that locale.
 *
 * - overlay leaf over base leaf     -> overlay wins
 * - overlay object over base object -> merged key by key
 * - key only in overlay             -> added
 * - key only in base                -> kept
 *
 * A shape disagreement (object over leaf, or leaf over object) resolves to the
 * overlay, because at runtime something has to render and the fork's value is
 * the more specific intent. It is also a mistake, so the audit reports it
 * rather than leaving it to be discovered by a blank label in the UI.
 */
export function mergeOverlay(
  base: TranslationTree,
  overlay?: TranslationTree | null,
): TranslationTree {
  const out: TranslationTree = {};

  // Own keys only, and never `__proto__`: a locale file is parsed JSON, so a
  // literal `"constructor"` or `"__proto__"` key is reachable from data.
  // Copying with a plain `for...in` over inherited properties, or assigning
  // `__proto__`, would let a translation file reach the prototype chain.
  for (const key of Object.keys(base)) {
    if (key === "__proto__") continue;
    out[key] = base[key];
  }

  if (!overlay) return out;

  for (const key of Object.keys(overlay)) {
    if (key === "__proto__") continue;
    const patch = overlay[key];
    const current = out[key];
    out[key] =
      isMergeable(current) && isMergeable(patch)
        ? mergeOverlay(current, patch)
        : patch;
  }

  return out;
}

/**
 * Turn the two `import.meta.glob` results into i18next's per-language
 * resources, pairing each `locales/<lang>/translation.json` with the
 * `locales/<lang>/plus.json` beside it.
 *
 * Lives here rather than inline in index.ts so that it is reachable from a
 * plain script: index.ts pulls in the Tauri plugins and cannot be imported
 * outside the app, and a wiring step that only the app can run is a wiring
 * step nothing verifies.
 *
 * The language is the directory name, taken structurally rather than by
 * pattern-matching the path, because the two globs have to agree on it - an
 * overlay that attaches to a slightly different key than its base file
 * attaches to nothing at all, silently, and the app renders upstream's
 * English with no error anywhere.
 */
export function buildLocaleResources(
  localeModules: Record<string, { default: TranslationTree }>,
  overlayModules: Record<string, { default: TranslationTree }>,
): Record<string, TranslationTree> {
  const localeOf = (path: string) => path.split("/").slice(-2, -1)[0];

  const overlays: Record<string, TranslationTree> = {};
  for (const [path, module] of Object.entries(overlayModules)) {
    const lang = localeOf(path);
    if (lang) overlays[lang] = module.default;
  }

  const resources: Record<string, TranslationTree> = {};
  for (const [path, module] of Object.entries(localeModules)) {
    const lang = localeOf(path);
    if (lang) resources[lang] = mergeOverlay(module.default, overlays[lang]);
  }
  return resources;
}

/** Every leaf path in a translation tree, as `["settings", "about", "title"]`. */
export function leafPaths(
  tree: TranslationTree,
  prefix: string[] = [],
): string[][] {
  const out: string[][] = [];
  for (const key of Object.keys(tree)) {
    const value = tree[key];
    const path = [...prefix, key];
    if (isMergeable(value)) out.push(...leafPaths(value, path));
    else out.push(path);
  }
  return out;
}

/** The value at `path`, or `undefined` if any segment is missing. */
export function valueAt(
  tree: TranslationTree,
  path: string[],
): TranslationNode | TranslationNode[] | undefined {
  let current: unknown = tree;
  for (const key of path) {
    if (!isMergeable(current) || !hasOwn(current, key)) return undefined;
    current = (current as TranslationTree)[key];
  }
  return current as TranslationNode;
}

/**
 * The product name this fork ships under, and the upstream name it replaces.
 *
 * Applied as a whole-word substitution that refuses to touch a "Handy" that is
 * already followed by "Plus", which makes it idempotent: re-running it over an
 * already-rebranded string is a no-op rather than "Handy Plus Plus".
 */
export const UPSTREAM_PRODUCT_NAME = "Handy";
export const PRODUCT_NAME = "Handy Plus";

export function rebrand(text: string): string {
  return text.replace(/\bHandy\b(?! Plus)/g, PRODUCT_NAME);
}
