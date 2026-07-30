# Handy Plus: Apple-style visual redesign

## Context

The user wants Handy Plus to look and feel like a native, polished Apple/macOS app — referencing Apple's design language and the quality bar Steve Jobs was known for demanding — without breaking the daily automated upstream sync from `cjpais/Handy` (a standing constraint documented in `CLAUDE.md`: avoid changes that needlessly increase merge-conflict risk with files CJ actively maintains).

Research findings that shape this plan:
- **Tailwind v4, CSS-first config.** No `tailwind.config.*` exists. All theme tokens (colors) live in `src/styles/theme.css` + an `@theme inline` block in `src/App.css`. Tailwind v4 lets a project override *any* default-namespace variable (colors, radius, shadow, spacing, easing, fonts) in this `@theme` block, and every utility class using that token updates globally with zero per-component edits. This is the single highest-leverage, lowest-risk lever available — most of the visual shift should come from here.
- **Theme switching** = `data-theme="light"|"dark"` attribute on `<html>`, set by `src/lib/utils/theme.ts`. Untouched by this plan.
- **Design system layer**: `src/components/ui/` (Button, Input, Textarea, ToggleSwitch, SettingContainer, SettingsGroup, Dropdown, Select, Slider, Tooltip, Badge, Alert, Dialog, AudioPlayer, etc.) — ~40 of 107 `.tsx` files already import from here. Current baseline: flat hairline-border cards (`rounded-lg`, `border-mid-gray/20`), no shadows anywhere, `transition-colors` only, no custom radius/spacing scale, single pink accent (`--color-background-ui: #da5893`) against neutral grays.
- **~13 files bypass the primitives** with raw hand-styled `<button>`s (`ModelsSettings.tsx` ×4, `UpdateChecker.tsx` ×3, `HistorySettings.tsx`, `InsightsSettings.tsx`, `Footer.tsx`, `Onboarding.tsx`, `AccessibilityPermissions.tsx`, `AccessibilityOnboarding.tsx`, `SecureInputWarning.tsx`, `ModelStatusButton.tsx`, `LanguageSelector.tsx`, `KeyboardDiagnostic.tsx`). Restyling `ui/` alone won't reach these — they need individual, surgical swaps to stay visually consistent.
- **No custom font** — the app already relies on Tailwind's `system-ui` stack, which resolves to San Francisco on macOS "for free." No font work needed.
- **Native OS window chrome** (no custom titlebar, `decorations` not overridden). Building a custom titlebar would mean editing `src-tauri/src/lib.rs`'s window builder — a file CJ actively maintains, and the single highest-merge-risk change available for this task.
- **Not yet checked**: the separate recording-overlay HUD (`src/overlay/RecordingOverlay.css`) has its own scoped styling, outside `App.css`'s token system — a highly-visible surface (the transcription-in-progress indicator) that a v1 pass of this plan risked leaving inconsistent. Flagged for the revised plan.

## Decisions made (confident calls, per the user's "implement if you're sure" latitude)

1. **Keep the pink brand accent** (`--color-background-ui` / `--color-logo-primary`). This redesign refines *execution* — depth, hierarchy, motion, consistency — not brand identity.
2. **Keep native OS window chrome — do not build a custom titlebar.** Native chrome on macOS already *is* an authentic Apple window frame, at zero merge risk. Highest-risk, most upstream-conflict-prone change available, for debatable gain. Revisit only as its own separately-approved decision later.

## Self-critique of the first draft (honest rating: 7/10)

Strong on architecture (correctly found the highest-leverage lever) and on defensible taste calls, but fell short of "bug-proof" on:
- No concrete values — "bump radius," "add shadows" isn't a spec, it's a direction.
- No dark-mode-aware shadow design (a shadow tuned for light mode often reads as a muddy black smear on an already-dark background — a real regression risk, not just a preference).
- No WCAG contrast verification for any new color pairing — "looks nice" isn't the same as "still legible."
- Never empirically verified the Tailwind v4 `@theme` override actually works as claimed in *this* project's build — an assumption from training knowledge, not from running it.
- Skipped the recording-overlay HUD entirely — arguably the single most-seen surface in the whole app.
- No explicit call on whether the aesthetic should be macOS-only (`data-platform="macos"`) or unified across Windows/Linux too.
- No verification method given this sandbox has no display — needed a concrete substitute for "eyeball it."

## Verification work done before finalizing (real, run, with output — not aspirational)

1. **Empirically proved the Tailwind v4 override mechanism.** Added `@theme { --radius-lg: 0.875rem; }` to `App.css`, ran `bun run build`, grepped compiled `dist/assets/main-*.css`:
   - `.rounded-lg{border-radius:var(--radius-lg)}` — confirmed the utility references the variable.
   - `--radius-lg:.875rem;` — confirmed the overridden value is what's actually emitted at `:root`.
   Reverted the throwaway change immediately after (`diff` confirmed a clean revert). The core lever this whole plan depends on is proven against *this exact project's build*, not assumed from training knowledge.

2. **Computed real WCAG contrast ratios** (proper relative-luminance formula, sRGB→linear, run as an actual script — not eyeballed):
   - Existing light text/bg: **18.52:1**. Existing dark text/bg: **13.67:1**. (Both already excellent — nothing to fix here, confirms the baseline is safe to build on.)
   - Tested whether a light-mode "elevated surface" color token is even possible: `color-mix(fbfbfb 97%, white 3%)` → **byte-identical to `fbfbfb`**. There is no usable luminance headroom in light mode (background is already 98.4% of max white). **Conclusion: light mode gets no separate surface token — depth comes from border + shadow only**, not a fake color step that wouldn't render as different anyway.
   - Dark-mode elevated surface at 6% white mix (`color-mix(2c2b29 94%, white 6%)` → `#393836`): text-on-surface contrast **11.32:1** (comfortably safe), surface-on-background self-contrast **1.21:1** (a real, perceptible-but-subtle lift, not garish). This value is now locked in as verified, not guessed.

3. **Read `RecordingOverlay.css` directly, in full**, rather than assuming scope. Finding that changed the plan: it contains the explicit comment *"Flat: no drop shadow, no backdrop blur/saturate (was a glassy gradient look)"* — someone already tried a fancier look here and deliberately reverted it. The file is also tightly coupled to Rust-side window geometry (`overlay.rs` must stay in sync with its CSS custom properties, per its own comments) and already uses proper spring easing (`cubic-bezier(0.22, 1, 0.36, 1)`) and correct light/dark `color-mix` theming. **Revised decision: exclude the overlay entirely, don't touch it at all** — it's already at the quality bar this plan is aiming for elsewhere, and it's clearly one of the most actively hand-tuned files in the repo (high upstream-conflict risk for zero gain).

4. **Decided: unified aesthetic across macOS/Windows/Linux**, not macOS-gated. Small indie fork, not a platform-mimicry app — consistency of its own identity beats chameleoning into three native look-and-feels (the Linear/Notion/Arc model), and halves the testing surface.

5. **Backward compatibility**: every change is a token *value* change or an additive CSS property (shadow, easing) on existing selectors in `theme.css`/`App.css`/`ui/*.tsx` — no class renames, no component prop API changes, no DOM structure changes. Code reading `rounded-lg`/`bg-mid-gray/X`/etc. keeps working identically; only rendered pixels shift. Trivially revertible (token values live in two small files) if the result doesn't land well on the user's own machine.

## Final token spec (measured, not showy — restraint over spectacle)

```css
/* Radius: a measured step up, not a dramatic swing into "iOS card" territory.
   Target reference is macOS System Settings' grouped-box radius, not iOS. */
--radius-md: 0.5rem;   /* 8px,  was 6px  (0.375rem) */
--radius-lg: 0.625rem; /* 10px, was 8px  (0.5rem)   */
--radius-xl: 0.875rem; /* 14px, was 12px (0.75rem)  */

/* Elevation — light and dark are NOT mirror images of each other, because a
   dark shadow is invisible-to-muddy on an already-dark background. */
--shadow-sm: 0 1px 2px rgba(0,0,0,0.04);
--shadow-md: 0 2px 8px rgba(0,0,0,0.06), 0 1px 2px rgba(0,0,0,0.04);
/* dark mode override (under existing :root[data-theme="dark"] / media query blocks): */
--shadow-sm: 0 1px 2px rgba(0,0,0,0.2);   /* still a real shadow, just tuned dark */
--shadow-md: 0 2px 10px rgba(0,0,0,0.28), 0 1px 2px rgba(0,0,0,0.2);

/* Elevated surface — dark mode only (see verification #2 above; light mode
   has no usable headroom, gets no separate token). */
--color-surface: var(--color-background); /* light: identical, depth via border+shadow */
/* dark mode override: */
--color-surface: color-mix(in srgb, var(--color-background) 94%, white 6%); /* verified #393836, 11.32:1 text contrast */

/* Motion — restrained standard easing, not spring/bounce (matches macOS's
   own restraint, avoids reading as gimmicky). */
--ease-apple: cubic-bezier(0.25, 0.1, 0.25, 1);
```

## Recommended approach — phased, ordered by leverage-to-risk ratio

### Phase A — Design tokens (`src/styles/theme.css`, `src/App.css`)
Implement the verified token spec above. Highest leverage, lowest risk: two small, self-contained files; cascades everywhere automatically via the empirically-proven `@theme` override mechanism.

### Phase B — Shared primitives (`src/components/ui/*.tsx`)
Cascades to ~40 files automatically.
- `Button.tsx`: new radius token; `shadow-sm` on primary (filled pink) variant only (secondary/ghost stay flat, matching how macOS distinguishes prominent vs. plain buttons); swap `transition-colors` → include the new easing token.
- `SettingsGroup.tsx` / `SettingContainer.tsx`: `--color-surface` + new radius + `shadow-sm` — the single biggest visual lever, nearly every screen is built from these two.
- `ToggleSwitch.tsx`: knob transition uses the new easing token, no structural change.
- `Input.tsx`, `Textarea.tsx`, `Dropdown.tsx`, `Select.tsx`, `Slider.tsx`, `Tooltip.tsx`, `Badge.tsx`, `Alert.tsx`, `Dialog.tsx`: radius/easing pass only, no structural changes.
- `overlay/RecordingOverlay.css`: **excluded** — see verification #3 above.

### Phase C — Bounded consistency pass (the ~13 ad-hoc files)
Small, surgical, per-file diffs — replace raw `<button className="...">` with `<Button variant="..." size="...">` only where a clean 1:1 swap exists. Skip anything needing bespoke layout rather than force it.

### Phase D — Typography hierarchy pass (optional, small)
`SettingContainer`/`SettingsGroup` text classes only (title/description/section-header sizing/weight). No new files, no font changes.

## Explicitly out of scope
- Custom titlebar / window vibrancy / `decorations(false)`.
- Any change to `src-tauri/src/lib.rs`, `overlay.rs`, or other Rust window/rendering code.
- `src/overlay/RecordingOverlay.css` and everything under `src/overlay/` — already excellent, deliberately flat by prior design decision, tightly coupled to Rust geometry. Not touched at all.
- Rebranding the accent color.
- Onboarding's own bespoke visuals beyond Phase C swaps, unless review surfaces something trivial.

## Verification
- `bun run lint && bun run build` after each phase.
- Contrast ratios computed and recorded above *before* implementation, not checked after the fact.
- The `@theme` override mechanism already empirically proven against this exact project's build (see above), not assumed.
- No Rust changes in this plan, so no `cargo check`/`cargo test` gate is required — run anyway before committing as a safety net, matching this session's established practice.
- Commit each phase separately so the redesign is easy to review/bisect if something doesn't land right on the user's own machine (still the only place this can actually be *seen*, since this sandbox has no display).
- Confirmed the shared `theme.css` (imported by both the main app and the recording-overlay bundle) leaks zero active styling into the overlay — only inert, unreferenced custom-property declarations. Checked the compiled overlay CSS directly for the new utility classes; found none. The overlay's rendered appearance is unaffected, as intended.

## Second-opinion consult (Fable, ~24k tokens)
Validated the core direction — specifically called out the asymmetric light/dark elevation logic as "the detail most AI redesigns get backwards," and the restrained single-easing-token choice as correct for a utility app. Two watchouts raised and resolved:
1. **Primary button contrast** — white text on the pink accent button measures 3.62:1, below the 4.5:1 AA threshold for body text. Verified this is **pre-existing** (unrelated to any token this redesign touched — the accent color and white text were never modified here). Flagged to the user rather than silently changed, since altering the brand accent's shade is a brand decision, not a bug fix within this plan's scope.
2. **`--radius-xl` (14px) risk of looking oversized on small controls** — checked usage directly: it's applied in exactly one place (`ModelCard.tsx`, a full onboarding card), never a small control. Non-issue.

## Phase C — files actually touched (bounded pass, no forced rewrites)
Applied the token-consistency touch (retune existing `transition-*` to the new easing; `bg-background` → `bg-surface` on floating/elevated panels) rather than swapping to the `Button` component, since none of these had a layout simple enough for a clean 1:1 swap without restructuring: `ModelsSettings.tsx`, `LanguageSelector.tsx`, `ModelDropdown.tsx`, `UpdateChecker.tsx` (including its hand-rolled portable-update modal, which also gained `shadow-xl` to match `Dialog.tsx`'s treatment), `Onboarding.tsx`, `AccessibilityOnboarding.tsx`, `ModelStatusButton.tsx`.

Deliberately left untouched: `AccessibilityPermissions.tsx`, `KeyboardDiagnostic.tsx`, `SecureInputWarning.tsx` — none of their buttons have an existing `transition` to retune, and adding one where none existed would be introducing new behavior, not restyling existing behavior.

## Phase D — done
Added `text-text/60` to `SettingContainer`'s inline (non-tooltip) description paragraphs, for a clearer primary/secondary label hierarchy matching Apple HIG convention. `SettingsGroup`'s section description already used `text-mid-gray` (already appropriately secondary) — left alone.

## Status: complete
All four phases implemented, verified (`lint`, `build`, `cargo check`, `cargo test` — 147/147), and committed. Everything in "Explicitly out of scope" above remains untouched.
