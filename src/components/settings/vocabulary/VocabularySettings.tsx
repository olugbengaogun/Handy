import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowRight, Check, GraduationCap, X } from "lucide-react";
import {
  commands,
  type LearningSuggestion,
  type TermSuggestion,
} from "@/bindings";
import { useSettings } from "../../../hooks/useSettings";
import { CorrectionPairs } from "../CorrectionPairs";
import { CustomWords } from "../CustomWords";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { Button } from "../../ui/Button";
import { Alert } from "../../ui/Alert";
import Badge from "../../ui/Badge";

/**
 * Everything Handy Plus knows about how this person talks, in one place.
 *
 * Four views of one idea: corrections it suggests from your edits, the
 * dictionary of corrections it applies, the words you told it to listen for,
 * and words it noticed you use often. These were previously split between
 * Advanced and nowhere — `correction_pairs` in particular was rendered as two
 * unrelated features. A system that learns from you is only trustworthy if you
 * can see, in one screen, everything it has concluded.
 */

/** A single `before → after` pair, rendered the same way in both lists. */
const CorrectionRow: React.FC<{
  suggestion: LearningSuggestion;
  countLabel: string;
  children?: React.ReactNode;
}> = ({ suggestion, countLabel, children }) => (
  <div className="flex items-center gap-3 px-4 py-3 flex-wrap">
    <div className="flex items-center gap-2 min-w-0 flex-1">
      <span className="text-sm text-text/60 line-through break-words">
        {suggestion.before}
      </span>
      <ArrowRight size={14} className="shrink-0 text-text/40" />
      <span className="text-sm font-medium break-words">
        {suggestion.after}
      </span>
    </div>
    <Badge variant="secondary">{countLabel}</Badge>
    {children}
  </div>
);

export const VocabularySettings: React.FC = () => {
  const { t } = useTranslation();
  const { refreshSettings } = useSettings();
  const [suggestions, setSuggestions] = useState<LearningSuggestion[]>([]);
  const [candidates, setCandidates] = useState<TermSuggestion[]>([]);
  const [busyTerm, setBusyTerm] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    // Cleared up front: without this a transient failure leaves the banner
    // pinned to the screen even after a later load succeeds, so the UI would
    // report a problem that no longer exists.
    setError(null);
    try {
      // Fetched together so the screen is always internally consistent: a
      // promotion moves a row from suggestions to rules, and accepting a mined
      // term removes it from the candidate list. Showing one of those updated
      // and not the others would be worse than a slightly slower load.
      const [pending, mined] = await Promise.all([
        commands.getLearningSuggestions(null),
        commands.getDictionaryCandidates(),
      ]);

      if (pending.status === "ok") {
        setSuggestions(pending.data);
      } else {
        setError(String(pending.error));
      }

      if (mined.status === "ok") {
        setCandidates(mined.data);
      } else {
        setError(String(mined.error));
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Both actions reload rather than mutating local state, so what is shown is
  // always what the database actually holds. These lists are short and the user
  // acts on them rarely; correctness is worth more than saving a round trip.
  const act = useCallback(
    async (id: number, action: "promote" | "dismiss") => {
      setBusyId(id);
      setError(null);
      try {
        const run =
          action === "promote"
            ? commands.promoteLearningSuggestion
            : commands.dismissLearningSuggestion;
        const result = await run(id);

        if (result.status === "error") {
          setError(String(result.error));
          return;
        }

        // Promoting and removing both rewrite `correction_pairs` on the backend,
        // and `write_settings` emits no event. Without this refresh the
        // frontend's cached settings — and so the dictionary shown under
        // Advanced — would silently disagree with what is actually stored.
        await Promise.all([load(), refreshSettings()]);
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyId(null);
      }
    },
    [load, refreshSettings],
  );

  const addTerm = useCallback(
    async (term: string) => {
      setBusyTerm(term);
      setError(null);
      try {
        const result = await commands.acceptDictionaryCandidate(term);
        if (result.status === "error") {
          setError(String(result.error));
          return;
        }
        await Promise.all([load(), refreshSettings()]);
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyTerm(null);
      }
    },
    [load, refreshSettings],
  );

  if (loading) {
    return (
      <div className="max-w-3xl w-full mx-auto">
        <div className="text-sm text-text/60 px-4 py-6">
          {t("settings.vocabulary.loading")}
        </div>
      </div>
    );
  }

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      {error && <Alert variant="error">{error}</Alert>}

      <SettingsGroup
        title={t("settings.vocabulary.suggestions.title")}
        description={t("settings.vocabulary.suggestions.description")}
      >
        {suggestions.length === 0 ? (
          // The empty state explains the mechanism rather than just reporting
          // emptiness — for most users this screen is empty the first time they
          // open it, and that is the moment to say how it fills up.
          <div className="px-4 py-6 flex items-start gap-3">
            <GraduationCap size={20} className="shrink-0 mt-0.5 text-text/40" />
            <p className="text-sm text-text/60">
              {t("settings.vocabulary.suggestions.empty")}
            </p>
          </div>
        ) : (
          suggestions.map((suggestion) => (
            <CorrectionRow
              key={suggestion.id}
              suggestion={suggestion}
              countLabel={t("settings.vocabulary.timesFixed", {
                count: suggestion.occurrences,
              })}
            >
              <div className="flex gap-2">
                <Button
                  size="sm"
                  variant="primary"
                  disabled={busyId === suggestion.id}
                  onClick={() => void act(suggestion.id, "promote")}
                  aria-label={t("settings.vocabulary.suggestions.acceptLabel", {
                    before: suggestion.before,
                    after: suggestion.after,
                  })}
                >
                  <Check size={14} />
                  {t("settings.vocabulary.suggestions.accept")}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={busyId === suggestion.id}
                  onClick={() => void act(suggestion.id, "dismiss")}
                  aria-label={t(
                    "settings.vocabulary.suggestions.dismissLabel",
                    {
                      before: suggestion.before,
                      after: suggestion.after,
                    },
                  )}
                >
                  <X size={14} />
                  {t("settings.vocabulary.suggestions.dismiss")}
                </Button>
              </div>
            </CorrectionRow>
          ))
        )}
      </SettingsGroup>

      {/* The manual half of the same concept. Previously these lived under
          Advanced → Transcription, which split one idea — "what Handy Plus
          knows about how I talk" — across two tabs, with `correction_pairs`
          rendered as two unrelated features. */}
      <SettingsGroup
        title={t("settings.vocabulary.dictionary.title")}
        description={t("settings.vocabulary.dictionary.description")}
      >
        <CorrectionPairs descriptionMode="tooltip" grouped />
      </SettingsGroup>

      <SettingsGroup
        title={t("settings.vocabulary.words.title")}
        description={t("settings.vocabulary.words.description")}
      >
        <CustomWords descriptionMode="tooltip" grouped />
      </SettingsGroup>

      <SettingsGroup
        title={t("settings.vocabulary.candidates.title")}
        description={t("settings.vocabulary.candidates.description")}
      >
        {candidates.length === 0 ? (
          <div className="px-4 py-6">
            <p className="text-sm text-text/60">
              {t("settings.vocabulary.candidates.empty")}
            </p>
          </div>
        ) : (
          candidates.map((candidate) => (
            <div
              key={candidate.term}
              className="flex items-center gap-3 px-4 py-3 flex-wrap"
            >
              <span className="text-sm font-medium min-w-0 flex-1 break-words">
                {candidate.term}
              </span>
              <Badge variant="secondary">
                {t("settings.vocabulary.candidates.timesSeen", {
                  count: candidate.occurrences,
                })}
              </Badge>
              <Button
                size="sm"
                variant="secondary"
                disabled={busyTerm === candidate.term}
                onClick={() => void addTerm(candidate.term)}
                aria-label={t("settings.vocabulary.candidates.addLabel", {
                  term: candidate.term,
                })}
              >
                <Check size={14} />
                {t("settings.vocabulary.candidates.add")}
              </Button>
            </div>
          ))
        )}
      </SettingsGroup>
    </div>
  );
};
