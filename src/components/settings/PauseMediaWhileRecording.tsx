import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";
import { commands } from "@/bindings";

interface PauseMediaWhileRecordingProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/**
 * Pause Spotify for the take, rather than only muting the output device.
 *
 * Enabling this sends one harmless Apple Event straight away. That is the whole
 * point of the probe: macOS raises its Automation consent prompt on the *first*
 * event, and without this it would arrive mid-sentence on the user's first
 * dictation — a modal stealing focus at the exact moment they are speaking.
 * Asking here, while they are looking at the switch they just flipped, is when
 * a permission prompt makes sense. The answer is reported inline, so a denied
 * permission reads as a denied permission instead of a feature that does
 * nothing.
 */
export const PauseMediaWhileRecording: React.FC<PauseMediaWhileRecordingProps> =
  React.memo(({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const [status, setStatus] = React.useState<string | null>(null);

    const enabled = getSetting("pause_media_while_recording") ?? false;

    const onChange = async (next: boolean) => {
      await updateSetting("pause_media_while_recording", next);
      if (!next) {
        setStatus(null);
        return;
      }
      // The generated binding re-throws anything that is already an Error
      // rather than folding it into the Result, so this genuinely can reject.
      // An unhandled rejection here would leave the switch on with no
      // explanation of why nothing happens when you record.
      try {
        const res = await commands.probeMediaControl();
        if (res.status === "error") {
          setStatus("error");
        } else {
          // "ok" needs no message: the switch being on already says it works.
          setStatus(res.data === "ok" ? null : res.data);
        }
      } catch {
        setStatus("error");
      }
    };

    return (
      <div className={grouped ? undefined : "w-full"}>
        <ToggleSwitch
          checked={enabled}
          onChange={onChange}
          isUpdating={isUpdating("pause_media_while_recording")}
          label={t("settings.sound.pauseMediaWhileRecording.label")}
          description={t("settings.sound.pauseMediaWhileRecording.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
        {enabled && status && (
          <p className="text-xs text-text/60 px-4 pb-2 -mt-1">
            {t(
              status === "denied"
                ? "settings.sound.pauseMediaWhileRecording.denied"
                : status === "not_running"
                  ? "settings.sound.pauseMediaWhileRecording.notRunning"
                  : status === "unsupported"
                    ? "settings.sound.pauseMediaWhileRecording.unsupported"
                    : "settings.sound.pauseMediaWhileRecording.failed",
            )}
          </p>
        )}
      </div>
    );
  });
