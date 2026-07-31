import React from "react";
import { useTranslation } from "react-i18next";
import { Dialog } from "../ui";
import { MarkdownContent } from "./MarkdownContent";
import type { ReleaseNote } from "./releaseNotes";

interface WhatsNewHistoryModalProps {
  notes: ReleaseNote[];
  open: boolean;
  onClose: () => void;
}

/**
 * The full release history, newest first. Distinct from `WhatsNewModal`, which
 * shows a single note after an update and marks it as seen - opening the
 * history is a deliberate act and deliberately does not touch that state.
 */
export const WhatsNewHistoryModal: React.FC<WhatsNewHistoryModalProps> = ({
  notes,
  open,
  onClose,
}) => {
  const { t } = useTranslation();

  return (
    <Dialog
      open={open}
      title={t("whatsNew.historyTitle")}
      closeLabel={t("common.close")}
      onOpenChange={(nextOpen) => {
        if (!nextOpen) onClose();
      }}
    >
      {notes.length === 0 ? (
        <p className="text-sm text-mid-gray">{t("whatsNew.historyEmpty")}</p>
      ) : (
        <div className="space-y-8">
          {notes.map((note) => (
            <section key={note.version} className="space-y-2">
              <h3 className="text-sm font-semibold text-text/80 border-b border-mid-gray/20 pb-1">
                {t("whatsNew.historyVersionHeading", { version: note.version })}
              </h3>
              <MarkdownContent markdown={note.markdown} />
            </section>
          ))}
        </div>
      )}
    </Dialog>
  );
};
