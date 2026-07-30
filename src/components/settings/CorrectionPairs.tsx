import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useSettings } from "../../hooks/useSettings";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { SettingContainer } from "../ui/SettingContainer";
import type { CorrectionPair } from "@/bindings";

interface CorrectionPairsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const sanitize = (value: string) => value.trim().replace(/[<>"']/g, "");

export const CorrectionPairs: React.FC<CorrectionPairsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const [wrong, setWrong] = useState("");
    const [correct, setCorrect] = useState("");
    const correctionPairs = getSetting("correction_pairs") || [];

    const handleAddPair = () => {
      const sanitizedWrong = sanitize(wrong);
      const sanitizedCorrect = sanitize(correct);
      if (
        !sanitizedWrong ||
        !sanitizedCorrect ||
        sanitizedWrong.length > 50 ||
        sanitizedCorrect.length > 50
      ) {
        return;
      }
      if (
        correctionPairs.some(
          (pair) => pair.wrong.toLowerCase() === sanitizedWrong.toLowerCase(),
        )
      ) {
        toast.error(
          t("settings.advanced.correctionPairs.duplicate", {
            word: sanitizedWrong,
          }),
        );
        return;
      }
      updateSetting("correction_pairs", [
        ...correctionPairs,
        { wrong: sanitizedWrong, correct: sanitizedCorrect },
      ]);
      setWrong("");
      setCorrect("");
    };

    const handleRemovePair = (pairToRemove: CorrectionPair) => {
      updateSetting(
        "correction_pairs",
        correctionPairs.filter(
          (pair) =>
            !(
              pair.wrong === pairToRemove.wrong &&
              pair.correct === pairToRemove.correct
            ),
        ),
      );
    };

    const handleKeyPress = (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        handleAddPair();
      }
    };

    const isBusy = isUpdating("correction_pairs");

    return (
      <>
        <SettingContainer
          title={t("settings.advanced.correctionPairs.title")}
          description={t("settings.advanced.correctionPairs.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div className="flex items-center gap-2">
            <Input
              type="text"
              className="max-w-32"
              value={wrong}
              onChange={(e) => setWrong(e.target.value)}
              onKeyDown={handleKeyPress}
              placeholder={t(
                "settings.advanced.correctionPairs.wrongPlaceholder",
              )}
              variant="compact"
              disabled={isBusy}
            />
            <span className="text-text/40 text-sm">→</span>
            <Input
              type="text"
              className="max-w-32"
              value={correct}
              onChange={(e) => setCorrect(e.target.value)}
              onKeyDown={handleKeyPress}
              placeholder={t(
                "settings.advanced.correctionPairs.correctPlaceholder",
              )}
              variant="compact"
              disabled={isBusy}
            />
            <Button
              onClick={handleAddPair}
              disabled={!wrong.trim() || !correct.trim() || isBusy}
              variant="primary"
              size="md"
            >
              {t("settings.advanced.correctionPairs.add")}
            </Button>
          </div>
        </SettingContainer>
        {correctionPairs.length > 0 && (
          <div
            className={`px-4 p-2 ${grouped ? "" : "rounded-lg border border-mid-gray/20"} flex flex-wrap gap-1`}
          >
            {correctionPairs.map((pair) => (
              <Button
                key={`${pair.wrong}->${pair.correct}`}
                onClick={() => handleRemovePair(pair)}
                disabled={isBusy}
                variant="secondary"
                size="sm"
                className="inline-flex items-center gap-1 cursor-pointer"
                aria-label={t("settings.advanced.correctionPairs.remove", {
                  word: pair.wrong,
                })}
              >
                <span>
                  {pair.wrong} → {pair.correct}
                </span>
                <svg
                  className="w-3 h-3"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M6 18L18 6M6 6l12 12"
                  />
                </svg>
              </Button>
            ))}
          </div>
        )}
      </>
    );
  },
);

CorrectionPairs.displayName = "CorrectionPairs";
