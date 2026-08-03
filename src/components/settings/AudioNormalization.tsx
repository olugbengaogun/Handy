import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface AudioNormalizationProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const AudioNormalization: React.FC<AudioNormalizationProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("audio_normalization") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("audio_normalization", value)}
        isUpdating={isUpdating("audio_normalization")}
        label={t("settings.advanced.audioNormalization.label")}
        description={t("settings.advanced.audioNormalization.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
