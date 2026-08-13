//! Rendering history rows into export formats.
//!
//! Deliberately free of database and filesystem access: everything here is
//! `rows in, String out`, so each format can be tested for quoting, escaping and
//! ordering without a temp database. `commands::history` does the querying and
//! the single file write.

use serde::Serialize;

/// One transcript, flattened for export.
///
/// `original` is the model's untouched first output, frozen at save time. It is
/// `None` for rows written before that column existed, and equal to `text` for
/// any transcript nobody has corrected.
#[derive(Debug, Clone)]
pub struct ExportRow {
    pub id: i64,
    /// Unix seconds.
    pub timestamp: i64,
    pub title: String,
    pub text: String,
    pub original: Option<String>,
    pub verified: bool,
    pub model_id: Option<String>,
    pub language: Option<String>,
    pub word_count: i64,
    pub duration_secs: f64,
}

/// One learned correction: a phrase the user fixed by hand, and how often.
#[derive(Debug, Clone)]
pub struct CorrectionRow {
    pub before: String,
    pub after: String,
    pub kind: String,
    pub occurrences: i64,
    pub model_id: Option<String>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Markdown,
    Csv,
    PlainText,
    TrainingJsonl,
}

impl ExportFormat {
    /// Extension for the save dialog's default filename.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Markdown => "md",
            ExportFormat::Csv => "csv",
            ExportFormat::PlainText => "txt",
            ExportFormat::TrainingJsonl => "jsonl",
        }
    }
}

/// Formats a unix timestamp as a local date, e.g. `2026-08-13`.
fn local_date(timestamp: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(timestamp, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d").to_string(),
        _ => "unknown".to_string(),
    }
}

/// Formats a unix timestamp as a local wall-clock time, e.g. `14:05`.
fn local_time(timestamp: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(timestamp, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%H:%M").to_string(),
        _ => "--:--".to_string(),
    }
}

/// Quotes a field for RFC 4180. Always quoted: a transcript can contain commas,
/// quotes and newlines, and unconditional quoting removes the chance of a rule
/// being applied to one field and forgotten on another.
fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Human-readable archive, grouped by day.
pub fn to_markdown(rows: &[ExportRow]) -> String {
    let mut out = String::new();
    out.push_str("# Handy Plus transcripts\n\n");

    if rows.is_empty() {
        out.push_str("_No transcripts matched._\n");
        return out;
    }

    let mut current_day = String::new();
    for row in rows {
        let day = local_date(row.timestamp);
        if day != current_day {
            out.push_str(&format!("\n## {day}\n\n"));
            current_day = day;
        }

        let mut meta = vec![
            local_time(row.timestamp),
            format!("{} words", row.word_count),
        ];
        if row.duration_secs > 0.0 {
            meta.push(format!("{:.0}s", row.duration_secs));
        }
        if row.verified {
            meta.push("edited by hand".to_string());
        }

        out.push_str(&format!("**{}**\n\n", meta.join(" · ")));
        out.push_str(row.text.trim());
        out.push_str("\n\n");
    }

    out
}

/// One row per transcript, every column, for spreadsheets.
pub fn to_csv(rows: &[ExportRow]) -> String {
    let mut out = String::from(
        "id,timestamp,date,time,title,text,original_text,verified,model_id,language,word_count,duration_secs\n",
    );

    for row in rows {
        let fields = [
            row.id.to_string(),
            row.timestamp.to_string(),
            local_date(row.timestamp),
            local_time(row.timestamp),
            row.title.clone(),
            row.text.clone(),
            row.original.clone().unwrap_or_default(),
            row.verified.to_string(),
            row.model_id.clone().unwrap_or_default(),
            row.language.clone().unwrap_or_default(),
            row.word_count.to_string(),
            format!("{:.2}", row.duration_secs),
        ];
        let line: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }

    out
}

/// Transcripts and nothing else, blank-line separated.
pub fn to_plain_text(rows: &[ExportRow]) -> String {
    rows.iter()
        .map(|row| row.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TrainingRecord<'a> {
    /// A whole transcript the user rewrote: model output paired with the text a
    /// human actually wanted.
    Transcript {
        id: i64,
        timestamp: i64,
        messy: &'a str,
        clean: &'a str,
        verified: bool,
        // `Option<&str>`, not `&Option<String>`: serde passes `&self.field` to
        // the skip predicate, and a doubly-borrowed Option needs a deref
        // coercion the inference engine cannot always resolve.
        #[serde(skip_serializing_if = "Option::is_none")]
        model_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<&'a str>,
        word_count: i64,
    },
    /// A single phrase the user corrected, and how many times it recurred.
    Phrase {
        before: &'a str,
        after: &'a str,
        kind: &'a str,
        occurrences: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        model_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<&'a str>,
    },
}

/// JSON Lines carrying both grains of correction data.
///
/// Two record types share one file, discriminated by `type`, because they are
/// two views of the same behaviour and a consumer filters on one line of code.
/// Whole-transcript pairs only exist where the frozen original differs from the
/// current text, so this half stays empty until corrections are made *after*
/// `original_text` was introduced — the phrase records carry the file until
/// then.
pub fn to_training_jsonl(rows: &[ExportRow], corrections: &[CorrectionRow]) -> String {
    let mut out = String::new();

    for row in rows {
        // No frozen original, or nobody changed anything: there is no pair here,
        // and emitting `messy == clean` would teach a model to do nothing.
        let Some(original) = row.original.as_deref() else {
            continue;
        };
        if original.trim() == row.text.trim() {
            continue;
        }

        let record = TrainingRecord::Transcript {
            id: row.id,
            timestamp: row.timestamp,
            messy: original,
            clean: &row.text,
            verified: row.verified,
            model_id: row.model_id.as_deref(),
            language: row.language.as_deref(),
            word_count: row.word_count,
        };
        if let Ok(line) = serde_json::to_string(&record) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    for correction in corrections {
        let record = TrainingRecord::Phrase {
            before: &correction.before,
            after: &correction.after,
            kind: &correction.kind,
            occurrences: correction.occurrences,
            model_id: correction.model_id.as_deref(),
            language: correction.language.as_deref(),
        };
        if let Ok(line) = serde_json::to_string(&record) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    out
}

/// Renders `rows` in `format`. `corrections` is only consulted for
/// [`ExportFormat::TrainingJsonl`].
pub fn render(format: ExportFormat, rows: &[ExportRow], corrections: &[CorrectionRow]) -> String {
    match format {
        ExportFormat::Markdown => to_markdown(rows),
        ExportFormat::Csv => to_csv(rows),
        ExportFormat::PlainText => to_plain_text(rows),
        ExportFormat::TrainingJsonl => to_training_jsonl(rows, corrections),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, text: &str, original: Option<&str>) -> ExportRow {
        ExportRow {
            id,
            timestamp: 1_767_225_600, // 2026-01-01T00:00:00Z
            title: format!("Entry {id}"),
            text: text.to_string(),
            original: original.map(|s| s.to_string()),
            verified: original.is_some(),
            model_id: Some("parakeet-unified-en-0.6b".to_string()),
            language: Some("en".to_string()),
            word_count: text.split_whitespace().count() as i64,
            duration_secs: 12.5,
        }
    }

    #[test]
    fn csv_quotes_commas_quotes_and_newlines() {
        let rows = vec![row(1, "he said \"hi\", then left\nand came back", None)];
        let csv = to_csv(&rows);
        // The embedded quote is doubled, and the whole field stays wrapped, so
        // the newline lives inside one cell rather than starting a new record.
        assert!(csv.contains("\"he said \"\"hi\"\", then left\nand came back\""));
        // Header plus exactly one record, however many newlines the text held.
        assert_eq!(csv.matches("parakeet-unified-en-0.6b").count(), 1);
    }

    #[test]
    fn csv_header_column_count_matches_every_row() {
        let rows = vec![row(1, "one", None), row(2, "two", Some("too"))];
        let csv = to_csv(&rows);
        let header_commas = csv.lines().next().unwrap().matches(',').count();
        assert_eq!(header_commas, 11, "12 columns means 11 separators");
    }

    #[test]
    fn markdown_groups_by_day_once() {
        let rows = vec![row(1, "first", None), row(2, "second", None)];
        let md = to_markdown(&rows);
        // Same timestamp for both, so the date heading appears exactly once.
        assert_eq!(md.matches("## ").count(), 1);
        assert!(md.contains("first"));
        assert!(md.contains("second"));
    }

    #[test]
    fn markdown_says_so_when_nothing_matched() {
        assert!(to_markdown(&[]).contains("No transcripts matched"));
    }

    #[test]
    fn plain_text_is_transcripts_only() {
        let rows = vec![row(1, "first", None), row(2, "second", None)];
        assert_eq!(to_plain_text(&rows), "first\n\nsecond");
    }

    #[test]
    fn training_skips_rows_with_no_recorded_original() {
        // Pre-migration rows carry NULL and cannot be paired.
        let rows = vec![row(1, "clean text", None)];
        assert_eq!(to_training_jsonl(&rows, &[]), "");
    }

    #[test]
    fn training_skips_rows_nobody_corrected() {
        // messy == clean would teach a model to make no change.
        let rows = vec![row(1, "same text", Some("same text"))];
        assert_eq!(to_training_jsonl(&rows, &[]), "");
    }

    #[test]
    fn training_emits_a_pair_when_the_text_was_changed() {
        let rows = vec![row(1, "recruitment plan", Some("recruitment park"))];
        let jsonl = to_training_jsonl(&rows, &[]);
        assert_eq!(jsonl.lines().count(), 1);
        assert!(jsonl.contains("\"type\":\"transcript\""));
        assert!(jsonl.contains("\"messy\":\"recruitment park\""));
        assert!(jsonl.contains("\"clean\":\"recruitment plan\""));
    }

    #[test]
    fn training_includes_phrase_pairs() {
        let corrections = vec![CorrectionRow {
            before: "recruitment park".to_string(),
            after: "recruitment plan".to_string(),
            kind: "rewrite".to_string(),
            occurrences: 3,
            model_id: None,
            language: Some("en".to_string()),
        }];
        let jsonl = to_training_jsonl(&[], &corrections);
        assert_eq!(jsonl.lines().count(), 1);
        assert!(jsonl.contains("\"type\":\"phrase\""));
        assert!(jsonl.contains("\"occurrences\":3"));
        // Absent metadata is omitted rather than serialised as null.
        assert!(!jsonl.contains("model_id"));
    }

    #[test]
    fn every_training_line_is_valid_json() {
        let rows = vec![row(1, "clean \"quoted\"\ntext", Some("messy text"))];
        let corrections = vec![CorrectionRow {
            before: "a\tb".to_string(),
            after: "a b".to_string(),
            kind: "rewrite".to_string(),
            occurrences: 1,
            model_id: None,
            language: None,
        }];
        let jsonl = to_training_jsonl(&rows, &corrections);
        assert_eq!(jsonl.lines().count(), 2);
        for line in jsonl.lines() {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("invalid JSON line {line:?}: {e}"));
        }
    }

    #[test]
    fn empty_input_produces_empty_output_for_line_formats() {
        assert_eq!(to_plain_text(&[]), "");
        assert_eq!(to_training_jsonl(&[], &[]), "");
        // CSV still carries its header, so the file opens as a valid table.
        assert_eq!(to_csv(&[]).lines().count(), 1);
    }

    #[test]
    fn extensions_match_formats() {
        assert_eq!(ExportFormat::Markdown.extension(), "md");
        assert_eq!(ExportFormat::Csv.extension(), "csv");
        assert_eq!(ExportFormat::PlainText.extension(), "txt");
        assert_eq!(ExportFormat::TrainingJsonl.extension(), "jsonl");
    }
}
