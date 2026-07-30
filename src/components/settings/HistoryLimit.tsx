import React from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "../../hooks/useSettings";
import { Input } from "../ui/Input";
import { SettingContainer } from "../ui/SettingContainer";

interface HistoryLimitProps {
  descriptionMode?: "tooltip" | "inline";
  grouped?: boolean;
}

export const HistoryLimit: React.FC<HistoryLimitProps> = ({
  descriptionMode = "inline",
  grouped = false,
}) => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();

  const historyLimit = getSetting("history_limit") ?? 5;
  const isUnlimited =
    (getSetting("recording_retention_period") ?? "preserve_limit") === "never";
  const busy =
    isUpdating("history_limit") || isUpdating("recording_retention_period");

  const handleChange = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const value = parseInt(event.target.value, 10);
    if (!isNaN(value) && value >= 0) {
      updateSetting("history_limit", value);
    }
  };

  const handleUnlimitedChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    updateSetting(
      "recording_retention_period",
      event.target.checked ? "never" : "preserve_limit",
    );
  };

  return (
    <SettingContainer
      title={t("settings.debug.historyLimit.title")}
      description={t("settings.debug.historyLimit.description")}
      descriptionMode={descriptionMode}
      grouped={grouped}
      layout="horizontal"
    >
      <div className="flex items-center space-x-3">
        <Input
          type="number"
          min="0"
          max="1000"
          value={historyLimit}
          onChange={handleChange}
          disabled={busy || isUnlimited}
          className="w-20"
        />
        <span className="text-sm text-text">
          {t("settings.debug.historyLimit.entries")}
        </span>
        <label className="flex items-center gap-1.5 text-sm text-text cursor-pointer">
          <input
            type="checkbox"
            checked={isUnlimited}
            onChange={handleUnlimitedChange}
            disabled={busy}
            className="cursor-pointer"
          />
          {t("settings.debug.historyLimit.unlimited")}
        </label>
      </div>
    </SettingContainer>
  );
};
