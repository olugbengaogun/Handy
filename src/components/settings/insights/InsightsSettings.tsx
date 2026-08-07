import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type DailyUsage,
  type StatsRange,
  type UsageStats,
} from "@/bindings";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { StreakGrid, WordsPerDayChart, WpmGauge } from "./charts";

const RANGES: StatsRange[] = ["today", "week", "month", "all_time"];

/**
 * Days of history behind the trend chart for each range.
 *
 * Capped at 90 rather than following "all time" literally. The panel is about
 * 640px wide, so 180 bars would be under 2px each with their gaps — a texture,
 * not a chart. The window is named in the panel heading so a capped view is
 * never mistaken for the whole record; the tiles above still cover all time.
 */
const RANGE_DAYS: Record<StatsRange, number> = {
  today: 14,
  week: 14,
  month: 30,
  all_time: 90,
};

/** A year of squares, like the calendar this is modelled on. */
const STREAK_DAYS = 364;

const StatTile: React.FC<{
  label: string;
  value: string;
  hint?: string;
}> = ({ label, value, hint }) => (
  <div className="flex-1 min-w-[130px] bg-mid-gray/[0.07] border border-mid-gray/20 rounded-xl px-4 py-3">
    <div className="text-xs text-text/60">{label}</div>
    {/* Proportional figures: tabular-nums is for numbers that align in a
        column, and makes a large standalone value look gappy. */}
    <div className="text-2xl font-semibold mt-0.5">{value}</div>
    {hint && <div className="text-[11px] text-text/40 mt-0.5">{hint}</div>}
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
 * Longest and current run of consecutive days with any dictation.
 *
 * `series` is oldest-first and already gap-filled, so consecutive days are
 * simply adjacent entries — no date arithmetic, and therefore no DST or
 * month-boundary edge cases to get wrong.
 *
 * Today not being dictated yet does not break the current streak: the day is
 * still in progress, so the run is measured from yesterday. Ending a streak at
 * breakfast because nothing has been said yet would be a lie about the past.
 */
function computeStreaks(series: DailyUsage[]): {
  current: number;
  longest: number;
} {
  let longest = 0;
  let run = 0;
  for (const day of series) {
    run = day.entries > 0 ? run + 1 : 0;
    if (run > longest) longest = run;
  }

  let current = 0;
  for (let i = series.length - 1; i >= 0; i--) {
    if (series[i].entries > 0) {
      current++;
    } else if (i === series.length - 1) {
      continue; // today, still in progress
    } else {
      break;
    }
  }

  return { current, longest };
}

export const InsightsSettings: React.FC = () => {
  const { t, i18n } = useTranslation();
  const [range, setRange] = useState<StatsRange>("week");
  const [stats, setStats] = useState<UsageStats | null>(null);
  const [trend, setTrend] = useState<DailyUsage[]>([]);
  const [streakSeries, setStreakSeries] = useState<DailyUsage[]>([]);
  /** True until the first load lands — the only time the panel is blank. */
  const [loading, setLoading] = useState(true);
  /** True during any load. Switching range dims the existing render rather
   *  than replacing it with a skeleton, so nothing jumps and the numbers stay
   *  readable until the new ones arrive. */
  const [refreshing, setRefreshing] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setRefreshing(true);
    (async () => {
      try {
        const [statsResult, trendResult, streakResult] = await Promise.all([
          commands.getUsageStats(range),
          commands.getUsageDaily(RANGE_DAYS[range]),
          commands.getUsageDaily(STREAK_DAYS),
        ]);
        if (cancelled) return;
        if (statsResult.status === "ok") setStats(statsResult.data);
        if (trendResult.status === "ok") setTrend(trendResult.data);
        if (streakResult.status === "ok") setStreakSeries(streakResult.data);
      } catch (error) {
        console.error("Failed to load usage stats:", error);
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
  }, [range]);

  const streaks = useMemo(() => computeStreaks(streakSeries), [streakSeries]);

  const rangeLabel = (r: StatsRange) =>
    t(`settings.insights.range.${r === "all_time" ? "allTime" : r}`);

  const words = (n: number) => t("settings.insights.wordsCount", { count: n });

  const hasData = stats !== null && stats.total_entries > 0;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.insights.title")}>
        <div className="px-4 py-4 flex flex-col gap-4">
          <div className="flex gap-1.5 flex-wrap">
            {RANGES.map((r) => (
              <button
                key={r}
                onClick={() => setRange(r)}
                aria-pressed={range === r}
                className={`px-3 py-1.5 text-sm rounded-lg transition-colors cursor-pointer ${
                  range === r
                    ? "bg-logo-primary/20 text-accent-text font-medium"
                    : "bg-mid-gray/10 text-text/70 hover:bg-mid-gray/20"
                }`}
              >
                {rangeLabel(r)}
              </button>
            ))}
          </div>

          {loading ? (
            <div className="text-sm text-text/60 py-6">
              {t("settings.insights.loading")}
            </div>
          ) : !hasData ? (
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
                  value={stats.total_words.toLocaleString()}
                />
                <StatTile
                  label={t("settings.insights.totalRecordings")}
                  value={stats.total_entries.toLocaleString()}
                />
                <StatTile
                  label={t("settings.insights.timeSpeaking")}
                  value={`${Math.round(stats.total_duration_secs / 60).toLocaleString()}m`}
                />
              </div>

              <div className="grid grid-cols-1 sm:grid-cols-[1fr_auto] gap-3">
                <Panel
                  title={t("settings.insights.wordsPerDay", {
                    days: RANGE_DAYS[range],
                  })}
                >
                  <WordsPerDayChart
                    data={trend}
                    locale={i18n.language}
                    emptyLabel={t("settings.insights.empty")}
                    unitLabel={words}
                    peakLabel={(n) =>
                      t("settings.insights.peak", { words: words(n) })
                    }
                  />
                </Panel>
                <div className="bg-mid-gray/[0.07] border border-mid-gray/20 rounded-xl px-4 py-3 flex items-center justify-center">
                  <WpmGauge
                    wpm={stats.average_wpm}
                    label={t("settings.insights.averageWpm")}
                  />
                </div>
              </div>

              <Panel
                title={t("settings.insights.streak", {
                  current: streaks.current,
                  longest: streaks.longest,
                })}
              >
                <StreakGrid
                  data={streakSeries}
                  locale={i18n.language}
                  noneLabel={t("settings.insights.noDictation")}
                  lessLabel={t("settings.insights.less")}
                  moreLabel={t("settings.insights.more")}
                  unitLabel={words}
                />
              </Panel>

              <p className="text-[11px] text-text/40">
                {t("settings.insights.footnote")}
              </p>
            </div>
          )}
        </div>
      </SettingsGroup>
    </div>
  );
};
