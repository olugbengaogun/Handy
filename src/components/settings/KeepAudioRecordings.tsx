import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface KeepAudioRecordingsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const KeepAudioRecordings: React.FC<KeepAudioRecordingsProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    // Matches the backend default. A fallback of `true` here would show the
    // toggle as on before settings load, which is the wrong way round to be
    // wrong about whether audio is being kept.
    const keepAudioRecordings = getSetting("keep_audio_recordings") ?? false;

    return (
      <ToggleSwitch
        checked={keepAudioRecordings}
        onChange={(enabled) => updateSetting("keep_audio_recordings", enabled)}
        isUpdating={isUpdating("keep_audio_recordings")}
        label={t("settings.advanced.keepAudioRecordings.label")}
        description={t("settings.advanced.keepAudioRecordings.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
        tooltipPosition="bottom"
      />
    );
  });

KeepAudioRecordings.displayName = "KeepAudioRecordings";
