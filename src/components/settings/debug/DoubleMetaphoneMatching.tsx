import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { useSettings } from "../../../hooks/useSettings";

interface DoubleMetaphoneMatchingProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const DoubleMetaphoneMatching: React.FC<DoubleMetaphoneMatchingProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("double_metaphone_matching") ?? false;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("double_metaphone_matching", value)}
        isUpdating={isUpdating("double_metaphone_matching")}
        label={t("settings.debug.doubleMetaphoneMatching.label")}
        description={t("settings.debug.doubleMetaphoneMatching.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  });
