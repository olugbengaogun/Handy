import React from "react";
import { useTranslation } from "react-i18next";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { useSettings } from "../../hooks/useSettings";
import { useOsType } from "../../hooks/useOsType";
import type { OverlayPosition, OverlayStyle } from "@/bindings";

interface ShowOverlayProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const ShowOverlay: React.FC<ShowOverlayProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const osType = useOsType();
    // Matches the Rust default (settings.rs default_overlay_style): Linux
    // hides the overlay by default, every other platform shows it live.
    // Used only while settings are still loading (getSetting returns
    // undefined then) - guessing the wrong platform's default would flash
    // an incorrect selection before real settings arrive.
    const defaultOverlayStyle: OverlayStyle =
      osType === "linux" ? "none" : "live";

    const styleOptions = [
      {
        value: "none",
        label: t("settings.advanced.overlay.style.options.none"),
      },
      {
        value: "minimal",
        label: t("settings.advanced.overlay.style.options.minimal"),
      },
      {
        value: "live",
        label: t("settings.advanced.overlay.style.options.live"),
      },
    ];

    const positionOptions = [
      {
        value: "bottom",
        label: t("settings.advanced.overlay.position.options.bottom"),
      },
      {
        value: "top",
        label: t("settings.advanced.overlay.position.options.top"),
      },
    ];

    const selectedStyle = (getSetting("overlay_style") ||
      defaultOverlayStyle) as OverlayStyle;
    // Only "top" and "bottom" are selectable; anything else (empty, or a legacy
    // "none" from before the position was retired) falls back to "bottom".
    const selectedPosition: OverlayPosition =
      getSetting("overlay_position") === "top" ? "top" : "bottom";

    return (
      <>
        <SettingContainer
          title={t("settings.advanced.overlay.style.title")}
          description={t("settings.advanced.overlay.style.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <Dropdown
            options={styleOptions}
            selectedValue={selectedStyle}
            onSelect={(value) =>
              updateSetting("overlay_style", value as OverlayStyle)
            }
            disabled={isUpdating("overlay_style")}
          />
        </SettingContainer>

        {selectedStyle !== "none" && (
          <SettingContainer
            title={t("settings.advanced.overlay.position.title")}
            description={t("settings.advanced.overlay.position.description")}
            descriptionMode={descriptionMode}
            grouped={grouped}
          >
            <Dropdown
              options={positionOptions}
              selectedValue={selectedPosition}
              onSelect={(value) =>
                updateSetting("overlay_position", value as OverlayPosition)
              }
              disabled={isUpdating("overlay_position")}
            />
          </SettingContainer>
        )}
      </>
    );
  },
);
