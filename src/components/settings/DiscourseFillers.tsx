import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface DiscourseFillersProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const DiscourseFillers: React.FC<DiscourseFillersProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("remove_discourse_fillers") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("remove_discourse_fillers", value)}
        isUpdating={isUpdating("remove_discourse_fillers")}
        label={t("settings.advanced.discourseFillers.label")}
        description={t("settings.advanced.discourseFillers.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
