import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { save } from "@tauri-apps/plugin-dialog";
import {
  commands,
  type ExportFormat,
  type ExportSummary,
} from "../../../bindings";
import { useSettings } from "../../../hooks/useSettings";

const FORMATS: { value: ExportFormat; extension: string }[] = [
  { value: "markdown", extension: "md" },
  { value: "csv", extension: "csv" },
  { value: "plain_text", extension: "txt" },
  { value: "training_jsonl", extension: "jsonl" },
];

/** Presets in days. `null` means every transcript ever recorded. */
const RANGES: { key: string; days: number | null }[] = [
  { key: "last7", days: 7 },
  { key: "last30", days: 30 },
  { key: "last90", days: 90 },
  { key: "all", days: null },
];

interface ExportDialogProps {
  /** Explicitly ticked entries. Empty means "no selection, use the range". */
  selectedIds: number[];
  onClose: () => void;
}

export const ExportDialog: React.FC<ExportDialogProps> = ({
  selectedIds,
  onClose,
}) => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const [format, setFormat] = useState<ExportFormat>("markdown");
  const [rangeKey, setRangeKey] = useState("all");
  const [verifiedOnly, setVerifiedOnly] = useState(false);
  const [selectionOnly, setSelectionOnly] = useState(selectedIds.length > 0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [summary, setSummary] = useState<ExportSummary | null>(null);

  const range = useMemo(
    () => RANGES.find((r) => r.key === rangeKey) ?? RANGES[3],
    [rangeKey],
  );

  // History is pruned after every transcription unless retention is "Never",
  // so an export can only ever contain what pruning has left. Saying so up
  // front beats handing someone a five-line file and letting them wonder.
  const retention = getSetting("recording_retention_period");
  const historyLimit = getSetting("history_limit");
  const pruningWarning = useMemo(() => {
    if (!retention || retention === "never") return null;
    if (retention === "preserve_limit") {
      // Settings may still be loading; fall back to the backend default rather
      // than rendering "undefined most recent transcripts".
      return t("settings.history.export.prunedByCount", {
        n: historyLimit ?? 5,
      });
    }
    return t("settings.history.export.prunedByAge");
  }, [retention, historyLimit, t]);

  const runExport = async () => {
    setBusy(true);
    setError(null);
    setSummary(null);

    try {
      const extension =
        FORMATS.find((f) => f.value === format)?.extension ?? "txt";
      const stamp = new Date().toISOString().slice(0, 10);
      const destination = await save({
        defaultPath: `handy-plus-transcripts-${stamp}.${extension}`,
        filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
      });

      // The user cancelled the save dialog; not an error worth reporting.
      if (!destination) {
        setBusy(false);
        return;
      }

      const useSelection = selectionOnly && selectedIds.length > 0;

      // Unix seconds, matching the timestamp column. `null` for "all time" so
      // the query simply omits the bound rather than guessing an epoch.
      //
      // The backend intersects the date range with the id list, so a range left
      // over from before the user started ticking would silently drop entries
      // they had explicitly chosen. The picker is disabled in that mode; this
      // makes the request match what the picker is showing.
      const from =
        useSelection || range.days === null
          ? null
          : Math.floor(Date.now() / 1000) - range.days * 86400;

      const result = await commands.exportHistory(
        format,
        from,
        null,
        useSelection ? selectedIds : null,
        verifiedOnly,
        destination,
      );

      if (result.status === "ok") {
        setSummary(result.data);
      } else {
        setError(result.error);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
      <div className="w-full max-w-md rounded-lg bg-background p-5 shadow-xl">
        <h2 className="mb-4 text-lg font-semibold text-text">
          {t("settings.history.export.title")}
        </h2>

        <label className="mb-1 block text-sm font-medium text-text">
          {t("settings.history.export.format")}
        </label>
        <select
          className="mb-4 w-full rounded border border-mid-gray/40 bg-background p-2 text-sm text-text"
          value={format}
          onChange={(e) => setFormat(e.target.value as ExportFormat)}
          disabled={busy}
        >
          {FORMATS.map((f) => (
            <option key={f.value} value={f.value}>
              {t(`settings.history.export.formats.${f.value}`)}
            </option>
          ))}
        </select>

        <label className="mb-1 block text-sm font-medium text-text">
          {t("settings.history.export.range")}
        </label>
        <select
          className="mb-4 w-full rounded border border-mid-gray/40 bg-background p-2 text-sm text-text"
          value={rangeKey}
          onChange={(e) => setRangeKey(e.target.value)}
          disabled={busy || (selectionOnly && selectedIds.length > 0)}
        >
          {RANGES.map((r) => (
            <option key={r.key} value={r.key}>
              {t(`settings.history.export.ranges.${r.key}`)}
            </option>
          ))}
        </select>

        {selectedIds.length > 0 && (
          <label className="mb-2 flex items-center gap-2 text-sm text-text">
            <input
              type="checkbox"
              checked={selectionOnly}
              onChange={(e) => setSelectionOnly(e.target.checked)}
              disabled={busy}
            />
            {t("settings.history.export.selectionOnly", {
              n: selectedIds.length,
            })}
          </label>
        )}

        <label className="mb-4 flex items-center gap-2 text-sm text-text">
          <input
            type="checkbox"
            checked={verifiedOnly}
            onChange={(e) => setVerifiedOnly(e.target.checked)}
            disabled={busy}
          />
          {t("settings.history.export.verifiedOnly")}
        </label>

        {pruningWarning && (
          <p className="mb-3 text-xs text-amber-600 dark:text-amber-400">
            {pruningWarning}
          </p>
        )}

        {format === "training_jsonl" && (
          <p className="mb-4 text-xs text-text/60">
            {t("settings.history.export.trainingNote")}
          </p>
        )}

        {error && (
          <p className="mb-3 text-sm text-red-500" role="alert">
            {error}
          </p>
        )}

        {summary && (
          <p className="mb-3 text-sm text-text" role="status">
            {t("settings.history.export.done", {
              transcripts: summary.transcripts,
              pairs: summary.training_pairs,
              corrections: summary.corrections,
            })}
          </p>
        )}

        <div className="flex justify-end gap-2">
          <button
            className="rounded px-3 py-2 text-sm text-text hover:bg-mid-gray/20"
            onClick={onClose}
            disabled={busy}
          >
            {summary
              ? t("settings.history.export.close")
              : t("settings.history.export.cancel")}
          </button>
          <button
            className="rounded bg-logo-primary px-3 py-2 text-sm font-medium text-white disabled:opacity-50"
            onClick={runExport}
            disabled={busy}
          >
            {busy
              ? t("settings.history.export.working")
              : t("settings.history.export.confirm")}
          </button>
        </div>
      </div>
    </div>
  );
};
