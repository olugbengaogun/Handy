pub mod audio;
pub mod constants;
pub mod corrections;
pub mod diff;
pub mod discourse;
pub mod invariants;
pub mod phonetic;
pub mod prompt;
pub mod terms;
pub mod text;
pub mod utils;
pub mod vad;

pub use audio::{
    is_microphone_access_denied, is_no_input_device_error, list_input_devices, list_output_devices,
    normalize_for_transcription, read_wav_samples, save_wav_file, verify_wav_file, AudioRecorder,
    CpalDeviceInfo, VadPolicy,
};
pub use corrections::{apply_correction_pairs_tracked, apply_outside_protected, split_protected};
pub use diff::{
    diff_transcripts, is_plausible_cleanup, tally_promotable, Edit, EditKind,
    DEFAULT_MAX_DIVERGENCE,
};
pub use discourse::remove_discourse_fillers;
pub use invariants::preserves_protected_values;
pub use phonetic::{
    agreement as phonetic_agreement, score_multiplier as phonetic_score_multiplier,
};
pub use prompt::{build_whisper_initial_prompt, WhisperPrompt};
pub use terms::{mine_candidates, TermCandidate};
pub use text::{apply_custom_words, apply_custom_words_with, filter_transcription_output};
pub use utils::get_cpal_host;
pub use vad::{SileroVad, VoiceActivityDetector};
