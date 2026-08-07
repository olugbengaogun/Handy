import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type StatsRange, type UsageStats } from "@/bindings";
import { SettingsGroup } from "../../ui/SettingsGroup";

const RANGES: StatsRange[] = ["today", "week", "month", "all_time"];

const StatTile: React.FC<{ label: string; value: string }> = ({
  label,
  value,
}) => (
  <div className="flex-1 min-w-[140px] bg-mid-gray/10 border border-mid-gray/20 rounded-lg px-4 py-3">
    <div className="text-xs text-text/60">{label}</div>
    <div className="text-2xl font-semibold tabular-nums">{value}</div>
  </div>
);

export const InsightsSettings: React.FC = () => {
  const { t } = useTranslation();
  const [range, setRange] = useState<StatsRange>("week");
  const [stats, setStats] = useState<UsageStats | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        const result = await commands.getUsageStats(range);
        if (cancelled) return;
        if (result.status === "ok") {
          setStats(result.data);
        }
      } catch (error) {
        console.error("Failed to load usage stats:", error);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [range]);

  const rangeLabel = (r: StatsRange) =>
    t(`settings.insights.range.${r === "all_time" ? "allTime" : r}`);

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("settings.insights.title")}>
        <div className="px-4 py-2 flex flex-col gap-4">
          <div className="flex gap-2">
            {RANGES.map((r) => (
              <button
                key={r}
                onClick={() => setRange(r)}
                className={`px-3 py-1.5 text-sm rounded-md transition-colors cursor-pointer ${
                  range === r
                    ? "bg-logo-primary text-white"
                    : "bg-mid-gray/10 text-text/70 hover:bg-mid-gray/20"
                }`}
              >
                {rangeLabel(r)}
              </button>
            ))}
          </div>

          {loading ? (
            <div className="text-sm text-text/60">
              {t("settings.insights.loading")}
            </div>
          ) : stats && stats.total_entries > 0 ? (
            <div className="flex flex-wrap gap-3">
              <StatTile
                label={t("settings.insights.totalWords")}
                value={stats.total_words.toLocaleString()}
              />
              <StatTile
                label={t("settings.insights.averageWpm")}
                value={Math.round(stats.average_wpm).toLocaleString()}
              />
              <StatTile
                label={t("settings.insights.totalRecordings")}
                value={stats.total_entries.toLocaleString()}
              />
            </div>
          ) : (
            <div className="text-sm text-text/40 italic">
              {t("settings.insights.empty")}
            </div>
          )}
        </div>
      </SettingsGroup>
    </div>
  );
};
