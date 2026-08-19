import React, { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { commands, type DailyUsage } from "@/bindings";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { Alert } from "../../ui/Alert";
import { StreakGrid, WordsPerDayChart, WpmGauge } from "./charts";
import {
  canGoBack,
  canGoForward,
  containsToday,
  formatPeriod,
  fromDayKey,
  type Period,
  type PeriodKind,
  periodBounds,
  shiftPeriod,
  startOfDay,
  toDayKey,
} from "./period";

const KINDS: PeriodKind[] = ["day", "week", "month", "year", "all"];

/** A year of squares, like the calendar this is modelled on. */
const STREAK_DAYS = 364;

/**
 * Above this many days the trend switches from one bar per day to one per week.
 * The panel is about 640px wide; 365 daily bars would be under 2px each, which
 * is a texture rather than a chart.
 */
const MAX_DAILY_BARS = 100;

const StatTile: React.FC<{ label: string; value: string }> = ({
  label,
  value,
}) => (
  <div className="flex-1 min-w-[130px] bg-mid-gray/[0.07] border border-mid-gray/20 rounded-xl px-4 py-3">
    <div className="text-xs text-text/60">{label}</div>
    {/* Proportional figures: tabular-nums is for numbers that align in a
        column, and makes a large standalone value look gappy. */}
    <div className="text-2xl font-semibold mt-0.5">{value}</div>
  </div>
);

const Panel: React.FC<{ title: string; children: React.ReactNode }> = ({
  title,
  children,
}) => (
  <div className="bg-mid-gray/[0.07] border border-mid-gray/20 rounded-xl px-4 py-3">
    <div className="text-xs text-text/60 mb-3">{title}</div>
    {children}
  </div>
);

/**
 * Collapse a long series into weekly buckets so the chart keeps readable bars.
 * The bucket is labelled with its first day, which is what the tooltip shows.
 */
function bucketWeekly(days: DailyUsage[]): DailyUsage[] {
  const out: DailyUsage[] = [];
  for (let i = 0; i < days.length; i += 7) {
    const chunk = days.slice(i, i + 7);
    out.push({
      day: chunk[0].day,
      words: chunk.reduce((s, d) => s + d.words, 0),
      entries: chunk.reduce((s, d) => s + d.entries, 0),
      duration_secs: chunk.reduce((s, d) => s + d.duration_secs, 0),
    });
  }
  return out;
}

/**
 * Longest run of consecutive days with any dictation, and the run ending now.
 *
 * The series is gap-filled, so consecutive days are simply adjacent entries —
 * no date arithmetic, and therefore no DST or month-boundary edge case.
 *
 * `current` is only meaningful when the series actually reaches today, which is
 * why the caller passes `includesToday`: a "current streak" while browsing
 * March 2025 would be a number about nothing.
 */
function computeStreaks(
  series: DailyUsage[],
  includesToday: boolean,
): { current: number | null; longest: number } {
  let longest = 0;
  let run = 0;
  for (const day of series) {
    run = day.entries > 0 ? run + 1 : 0;
    if (run > longest) longest = run;
  }

  if (!includesToday) return { current: null, longest };

  let current = 0;
  for (let i = series.length - 1; i >= 0; i--) {
    if (series[i].entries > 0) {
      current++;
    } else if (i === series.length - 1) {
      // Today, still in progress. Ending a streak at breakfast because nothing
      // has been said yet would be a lie about the past.
      continue;
    } else {
      break;
    }
  }
  return { current, longest };
}

export const InsightsSettings: React.FC = () => {
  const { t, i18n } = useTranslation();
  const locale = i18n.language;

  const [period, setPeriod] = useState<Period>({
    kind: "week",
    anchor: new Date(),
  });
  const [days, setDays] = useState<DailyUsage[]>([]);
  const [streakSeries, setStreakSeries] = useState<DailyUsage[]>([]);
  const [firstRecorded, setFirstRecorded] = useState<string | null>(null);
  /** True until the first load lands — the only time the panel is blank. */
  const [loading, setLoading] = useState(true);
  /** True during any load. Switching period dims the current render instead of
   *  flashing a skeleton, so the figures stay readable and nothing jumps. */
  const [refreshing, setRefreshing] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const bounds = useMemo(() => periodBounds(period, locale), [period, locale]);

  useEffect(() => {
    let cancelled = false;
    setRefreshing(true);
    (async () => {
      try {
        // The streak grid always trails the period's end, so navigating back a
        // year moves the calendar with everything else — but it must never
        // trail past today. `bounds.end` is the end of the *period*, which for
        // the current week, month or year is a date that has not happened yet.
        // Trailing that padded the grid with days that had not occurred and,
        // worse, broke the current streak outright: computeStreaks only exempts
        // the final cell as "today, still in progress", so a series ending next
        // Saturday hit an empty Friday and reported 0 for someone who had
        // dictated every day for a month. Clamping here fixes the count and the
        // calendar at once, and is a no-op for any period that already ended.
        const today = startOfDay(new Date());
        const streakEnd = bounds.end > today ? today : bounds.end;
        const streakStart = new Date(
          streakEnd.getFullYear(),
          streakEnd.getMonth(),
          streakEnd.getDate() - (STREAK_DAYS - 1),
        );
        const [main, streak] = await Promise.all([
          commands.getUsageRange(
            bounds.start ? toDayKey(bounds.start) : null,
            toDayKey(bounds.end),
          ),
          commands.getUsageRange(toDayKey(streakStart), toDayKey(streakEnd)),
        ]);
        if (cancelled) return;
        // A failure must not leave the previous period's figures sitting under
        // the new period's label — that is the panel telling you August's
        // numbers are July's. Better to say nothing than to say something
        // wrong.
        if (main.status === "error" || streak.status === "error") {
          const detail =
            main.status === "error"
              ? main.error
              : (streak as { error: string }).error;
          console.error("Failed to load usage stats:", detail);
          setError(String(detail));
          setDays([]);
          setStreakSeries([]);
          return;
        }
        setError(null);
        setDays(main.data.days);
        setFirstRecorded(main.data.first_recorded);
        setStreakSeries(streak.data.days);
      } catch (e) {
        if (cancelled) return;
        console.error("Failed to load usage stats:", e);
        setError(String(e));
        setDays([]);
        setStreakSeries([]);
      } finally {
        if (!cancelled) {
          setLoading(false);
          setRefreshing(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [bounds]);

  // Every headline figure is summed from the same series the chart draws, so a
  // tile and the bars beneath it cannot disagree about the period.
  const totals = useMemo(() => {
    const words = days.reduce((s, d) => s + d.words, 0);
    const entries = days.reduce((s, d) => s + d.entries, 0);
    const seconds = days.reduce((s, d) => s + d.duration_secs, 0);
    return {
      words,
      entries,
      seconds,
      wpm: seconds > 0 ? words / (seconds / 60) : 0,
    };
  }, [days]);

  const includesToday = useMemo(
    () => containsToday(period, locale),
    [period, locale],
  );
  const streaks = useMemo(
    () => computeStreaks(streakSeries, includesToday),
    [streakSeries, includesToday],
  );

  const trend = useMemo(
    () => (days.length > MAX_DAILY_BARS ? bucketWeekly(days) : days),
    [days],
  );
  const weekly = days.length > MAX_DAILY_BARS;

  const step = useCallback(
    (delta: number) => setPeriod((p) => shiftPeriod(p, delta)),
    [],
  );

  const selectKind = useCallback((kind: PeriodKind) => {
    // Re-anchor to today so switching granularity always lands somewhere real
    // rather than, say, "the week of a month you were browsing".
    setPeriod({ kind, anchor: startOfDay(new Date()) });
  }, []);

  const back = canGoBack(period, locale, firstRecorded);
  const forward = canGoForward(period, locale);

  const words = (n: number) => t("settings.insights.wordsCount", { count: n });
  const hasAnyHistory = firstRecorded !== null;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.insights.title")}>
        <div className="px-4 py-4 flex flex-col gap-4">
          {/* One filter row above everything it scopes. */}
          <div className="flex items-center justify-between gap-3 flex-wrap">
            <div className="flex gap-1.5 flex-wrap">
              {KINDS.map((k) => (
                <button
                  key={k}
                  onClick={() => selectKind(k)}
                  aria-pressed={period.kind === k}
                  className={`px-3 py-1.5 text-sm rounded-lg transition-colors cursor-pointer ${
                    period.kind === k
                      ? "bg-logo-primary/20 text-accent-text font-medium"
                      : "bg-mid-gray/10 text-text/70 hover:bg-mid-gray/20"
                  }`}
                >
                  {t(`settings.insights.kind.${k}`)}
                </button>
              ))}
            </div>

            {period.kind !== "all" && (
              <div className="flex items-center gap-1">
                <button
                  onClick={() => step(-1)}
                  disabled={!back}
                  aria-label={t("settings.insights.previousPeriod")}
                  className="p-1.5 rounded-md text-text/70 hover:bg-mid-gray/20 disabled:opacity-30 disabled:cursor-not-allowed transition-colors cursor-pointer"
                >
                  <ChevronLeft className="w-4 h-4" />
                </button>
                <span className="text-sm min-w-[120px] text-center">
                  {formatPeriod(period, locale, t)}
                </span>
                <button
                  onClick={() => step(1)}
                  disabled={!forward}
                  aria-label={t("settings.insights.nextPeriod")}
                  className="p-1.5 rounded-md text-text/70 hover:bg-mid-gray/20 disabled:opacity-30 disabled:cursor-not-allowed transition-colors cursor-pointer"
                >
                  <ChevronRight className="w-4 h-4" />
                </button>
              </div>
            )}
          </div>

          {error ? (
            <Alert variant="error">{error}</Alert>
          ) : loading ? (
            <div className="text-sm text-text/60 py-6">
              {t("settings.insights.loading")}
            </div>
          ) : !hasAnyHistory ? (
            <div className="text-sm text-text/40 italic py-6">
              {t("settings.insights.empty")}
            </div>
          ) : (
            <div
              className={`flex flex-col gap-4 transition-opacity duration-200 ${
                refreshing ? "opacity-50" : "opacity-100"
              }`}
            >
              <div className="flex flex-wrap gap-3">
                <StatTile
                  label={t("settings.insights.totalWords")}
                  value={totals.words.toLocaleString()}
                />
                <StatTile
                  label={t("settings.insights.totalRecordings")}
                  value={totals.entries.toLocaleString()}
                />
                <StatTile
                  label={t("settings.insights.timeSpeaking")}
                  value={`${Math.round(totals.seconds / 60).toLocaleString()}m`}
                />
              </div>

              <div className="grid grid-cols-1 sm:grid-cols-[1fr_auto] gap-3">
                <Panel
                  title={t(
                    weekly
                      ? "settings.insights.wordsPerWeek"
                      : "settings.insights.wordsPerDay",
                  )}
                >
                  <WordsPerDayChart
                    data={trend}
                    locale={locale}
                    emptyLabel={t("settings.insights.emptyPeriod")}
                    unitLabel={words}
                    peakLabel={(n) =>
                      t("settings.insights.peak", { words: words(n) })
                    }
                    dateLabel={
                      weekly
                        ? (day, loc) =>
                            t("settings.insights.weekOf", {
                              date: fromDayKey(day).toLocaleDateString(loc, {
                                month: "short",
                                day: "numeric",
                              }),
                            })
                        : undefined
                    }
                  />
                </Panel>
                <div className="bg-mid-gray/[0.07] border border-mid-gray/20 rounded-xl px-4 py-3 flex items-center justify-center">
                  <WpmGauge
                    wpm={totals.wpm}
                    label={t("settings.insights.averageWpm")}
                  />
                </div>
              </div>

              <Panel
                title={
                  streaks.current === null
                    ? t("settings.insights.longestOnly", {
                        longest: streaks.longest,
                      })
                    : t("settings.insights.streak", {
                        current: streaks.current,
                        longest: streaks.longest,
                      })
                }
              >
                <StreakGrid
                  data={streakSeries}
                  locale={locale}
                  noneLabel={t("settings.insights.noDictation")}
                  lessLabel={t("settings.insights.less")}
                  moreLabel={t("settings.insights.more")}
                  unitLabel={words}
                />
              </Panel>

              <p className="text-[11px] text-text/40">
                {t("settings.insights.footnote")}
                {firstRecorded &&
                  ` ${t("settings.insights.since", {
                    date: fromDayKey(firstRecorded).toLocaleDateString(locale, {
                      month: "long",
                      year: "numeric",
                    }),
                  })}`}
              </p>
            </div>
          )}
        </div>
      </SettingsGroup>
    </div>
  );
};
