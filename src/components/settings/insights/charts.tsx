import React, { useId, useMemo, useState } from "react";

/**
 * Chart primitives for the Insights panel.
 *
 * Hand-rolled inline SVG rather than a charting library, for three reasons:
 * no new dependency in a desktop app that ships its own bundle, no new file
 * for the upstream sync to collide on, and these are three fixed shapes rather
 * than a general plotting need.
 *
 * Colour follows the app's own tokens. Every series here is single-series
 * magnitude data, so the palette is *sequential* — one hue (the brand's 198
 * sky blue), more-is-darker — never categorical. There is no legend because
 * there is nothing to tell apart; the heading names the measure.
 */

/** Sequential ramp, light→dark, for the streak grid. */
const RAMP_LIGHT = ["#cbeaf8", "#8ecfee", "#3ba3d4", "#086d97"];
/** Dark mode is stepped separately against the dark surface, not flipped. */
const RAMP_DARK = ["#17485e", "#1f6d8f", "#4aa8cf", "#8cd5f2"];

function useIsDark(): boolean {
  const [dark, setDark] = useState(() => {
    if (typeof window === "undefined") return false;
    const forced = document.documentElement.getAttribute("data-theme");
    if (forced === "dark") return true;
    if (forced === "light") return false;
    return window.matchMedia("(prefers-color-scheme: dark)").matches;
  });

  React.useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const read = () => {
      const forced = document.documentElement.getAttribute("data-theme");
      setDark(forced ? forced === "dark" : mq.matches);
    };
    mq.addEventListener("change", read);
    // The theme toggle stamps data-theme on <html>, which no media query sees.
    const observer = new MutationObserver(read);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => {
      mq.removeEventListener("change", read);
      observer.disconnect();
    };
  }, []);

  return dark;
}

export interface DayPoint {
  day: string;
  words: number;
  entries: number;
}

/** Local-midnight parse. `new Date("2026-08-07")` is parsed as UTC and lands on
 *  the previous day west of Greenwich, which would shift every label. */
function parseLocalDay(day: string): Date {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y, m - 1, d);
}

function formatDay(day: string, locale: string): string {
  return parseLocalDay(day).toLocaleDateString(locale, {
    month: "short",
    day: "numeric",
  });
}

/**
 * Words dictated per day.
 *
 * A column chart: the job is comparing magnitude across a small ordered set,
 * and bars anchored to a zero baseline are the honest form for a count. Only
 * the extremes are labelled — a number on every bar is noise, and the tooltip
 * carries the rest.
 */
export const WordsPerDayChart: React.FC<{
  data: DayPoint[];
  locale: string;
  emptyLabel: string;
  unitLabel: (n: number) => string;
  peakLabel: (n: number) => string;
}> = ({ data, locale, emptyLabel, unitLabel, peakLabel }) => {
  const [hover, setHover] = useState<number | null>(null);
  const max = useMemo(() => Math.max(1, ...data.map((d) => d.words)), [data]);
  const total = useMemo(() => data.reduce((s, d) => s + d.words, 0), [data]);

  if (total === 0) {
    return <div className="text-sm text-text/40 italic py-6">{emptyLabel}</div>;
  }

  const H = 96;
  // A visible stub for a day with some dictation keeps "a little" distinct from
  // "none" — a 1px sliver reads as an axis artefact.
  const MIN = 3;
  const peak = data.reduce((b, d, i) => (d.words > data[b].words ? i : b), 0);

  return (
    <div className="relative">
      <div
        className="flex items-end gap-[2px] h-24"
        onMouseLeave={() => setHover(null)}
      >
        {data.map((d, i) => {
          const h = d.words === 0 ? 0 : Math.max(MIN, (d.words / max) * H);
          const active = hover === i;
          return (
            <div
              key={d.day}
              // The hit area is the full column height, not the bar: a 3px stub
              // for a quiet day is otherwise impossible to hover deliberately.
              className="flex-1 h-full flex items-end min-w-0 cursor-default focus:outline-none"
              tabIndex={0}
              // Tooltips enhance, they never gate: the same value is on the
              // element itself, so keyboard and screen-reader users reach it
              // without a pointer.
              title={`${unitLabel(d.words)} · ${formatDay(d.day, locale)}`}
              aria-label={`${unitLabel(d.words)} · ${formatDay(d.day, locale)}`}
              onMouseEnter={() => setHover(i)}
              onFocus={() => setHover(i)}
              onBlur={() => setHover(null)}
            >
              <div
                className="w-full rounded-t transition-colors"
                style={{
                  height: `${h}px`,
                  minHeight: d.words === 0 ? 2 : undefined,
                  background:
                    d.words === 0
                      ? "color-mix(in srgb, var(--color-mid-gray) 25%, transparent)"
                      : active
                        ? "var(--color-accent-text)"
                        : "var(--color-logo-primary)",
                }}
              />
            </div>
          );
        })}
      </div>

      {/* Selectively direct-labelled: the peak alone, so the chart states its
          own ceiling without a number on every bar. */}
      <div className="flex justify-between mt-2 text-[11px] text-text/50">
        <span className="tabular-nums">{formatDay(data[0].day, locale)}</span>
        <span className="text-text/60">{peakLabel(data[peak].words)}</span>
        <span className="tabular-nums">
          {formatDay(data[data.length - 1].day, locale)}
        </span>
      </div>

      {hover !== null && (
        <div className="absolute -top-1 left-0 right-0 flex justify-center pointer-events-none">
          <div className="px-2 py-1 rounded-md bg-surface border border-mid-gray/30 shadow-md text-xs whitespace-nowrap">
            <span className="font-medium tabular-nums">
              {unitLabel(data[hover].words)}
            </span>
            <span className="text-text/50">
              {" · "}
              {formatDay(data[hover].day, locale)}
            </span>
          </div>
        </div>
      )}
    </div>
  );
};

/**
 * Contribution-style streak grid: one cell per day, columns are weeks.
 *
 * Sequential ramp in four steps. The pale steps sit under 3:1 against the
 * surface, which is inherent to a heatmap's low end — the relief is that every
 * cell carries a hover tooltip with the exact number, so colour is never the
 * only way to read a value.
 */
export const StreakGrid: React.FC<{
  data: DayPoint[];
  locale: string;
  noneLabel: string;
  lessLabel: string;
  moreLabel: string;
  unitLabel: (n: number) => string;
}> = ({ data, locale, noneLabel, lessLabel, moreLabel, unitLabel }) => {
  const dark = useIsDark();
  const ramp = dark ? RAMP_DARK : RAMP_LIGHT;
  const empty = dark ? "#4a4947" : "#ececec";
  const [hover, setHover] = useState<DayPoint | null>(null);

  // Quartile thresholds over non-empty days only, so one huge day does not
  // flatten every ordinary one into the palest step.
  const thresholds = useMemo(() => {
    const vals = data
      .map((d) => d.words)
      .filter((v) => v > 0)
      .sort((a, b) => a - b);
    if (vals.length === 0) return [1, 2, 3];
    const q = (p: number) => vals[Math.floor((vals.length - 1) * p)];
    return [q(0.25), q(0.5), q(0.75)];
  }, [data]);

  const colorFor = (words: number) => {
    if (words <= 0) return empty;
    if (words <= thresholds[0]) return ramp[0];
    if (words <= thresholds[1]) return ramp[1];
    if (words <= thresholds[2]) return ramp[2];
    return ramp[3];
  };

  // Pad the head so the first column starts on a Sunday and rows stay weekdays.
  const padded = useMemo(() => {
    if (data.length === 0) return [];
    const lead = parseLocalDay(data[0].day).getDay();
    return [...Array(lead).fill(null), ...data] as (DayPoint | null)[];
  }, [data]);

  const weeks = useMemo(() => {
    const out: (DayPoint | null)[][] = [];
    for (let i = 0; i < padded.length; i += 7) out.push(padded.slice(i, i + 7));
    return out;
  }, [padded]);

  return (
    <div className="relative">
      <div className="flex gap-[3px] overflow-x-auto pb-1">
        {weeks.map((week, wi) => (
          <div key={wi} className="flex flex-col gap-[3px]">
            {Array.from({ length: 7 }).map((_, di) => {
              const d = week[di];
              const label = d
                ? `${unitLabel(d.words)} · ${formatDay(d.day, locale)}`
                : undefined;
              return (
                <div
                  key={di}
                  className="w-[11px] h-[11px] rounded-[2px] shrink-0"
                  style={{
                    background: d ? colorFor(d.words) : "transparent",
                  }}
                  // Colour alone never carries the value: the exact count rides
                  // on the cell for pointer, keyboard and assistive tech alike.
                  // That is also the required relief for the pale ramp steps,
                  // which sit under 3:1 against the surface by nature.
                  title={label}
                  aria-label={label}
                  tabIndex={d ? 0 : undefined}
                  onMouseEnter={() => d && setHover(d)}
                  onMouseLeave={() => setHover(null)}
                  onFocus={() => d && setHover(d)}
                  onBlur={() => setHover(null)}
                />
              );
            })}
          </div>
        ))}
      </div>

      <div className="flex items-center justify-end gap-1 mt-2 text-[11px] text-text/50">
        <span>{lessLabel}</span>
        <div
          className="w-[11px] h-[11px] rounded-[2px]"
          style={{ background: empty }}
          title={noneLabel}
        />
        {ramp.map((c) => (
          <div
            key={c}
            className="w-[11px] h-[11px] rounded-[2px]"
            style={{ background: c }}
          />
        ))}
        <span>{moreLabel}</span>
      </div>

      {hover && (
        <div className="absolute -top-1 left-0 right-0 flex justify-center pointer-events-none">
          <div className="px-2 py-1 rounded-md bg-surface border border-mid-gray/30 shadow-md text-xs whitespace-nowrap">
            <span className="font-medium tabular-nums">
              {unitLabel(hover.words)}
            </span>
            <span className="text-text/50">
              {" · "}
              {formatDay(hover.day, locale)}
            </span>
          </div>
        </div>
      )}
    </div>
  );
};

/**
 * Words-per-minute as an arc meter.
 *
 * A meter needs a scale or it is decoration, so the arc is drawn against an
 * explicit, labelled 0–220 wpm range — roughly the span of human speech, with
 * conversational pace around 130. Deliberately no percentile: this app has no
 * cohort to compare anyone against, and inventing "top 0.2%" is exactly the
 * kind of claim that makes a stats panel feel fake.
 */
export const WpmGauge: React.FC<{ wpm: number; label: string }> = ({
  wpm,
  label,
}) => {
  const gradientId = useId();
  const MAX = 220;
  const pct = Math.max(0, Math.min(1, wpm / MAX));

  // Semicircle, radius 52, centred at (60, 60).
  const R = 52;
  const circumference = Math.PI * R;

  return (
    <div className="flex flex-col items-center justify-center">
      <svg
        viewBox="0 0 120 72"
        className="w-[120px] h-[72px]"
        role="img"
        aria-label={`${label}: ${Math.round(wpm)}`}
      >
        <defs>
          <linearGradient id={gradientId} x1="0" y1="0" x2="1" y2="0">
            <stop offset="0%" stopColor="var(--color-logo-primary)" />
            <stop offset="100%" stopColor="var(--color-accent-text)" />
          </linearGradient>
        </defs>
        <path
          d={`M 8 60 A ${R} ${R} 0 0 1 112 60`}
          fill="none"
          stroke="var(--color-mid-gray)"
          strokeOpacity="0.2"
          strokeWidth="8"
          strokeLinecap="round"
        />
        <path
          d={`M 8 60 A ${R} ${R} 0 0 1 112 60`}
          fill="none"
          stroke={`url(#${gradientId})`}
          strokeWidth="8"
          strokeLinecap="round"
          strokeDasharray={`${pct * circumference} ${circumference}`}
        />
      </svg>
      <div className="-mt-6 text-center">
        {/* Proportional figures, not tabular: equal-width digits make a large
            standalone number look loosely spaced. tabular-nums is for columns
            that align vertically, which this does not. */}
        <div className="text-2xl font-semibold leading-none">
          {Math.round(wpm).toLocaleString()}
        </div>
        <div className="text-xs text-text/60 mt-1">{label}</div>
      </div>
    </div>
  );
};
