//! Loudness normalisation for captured audio.
//!
//! Until now nothing touched the samples between capture and the model: the
//! recorder downmixed to mono and resampled to 16 kHz, and raw `f32` went
//! straight to the engine. That is fine for a well-set-up microphone and poor
//! for a quiet one, where the signal sits so far below full scale that the
//! model is effectively working with fewer bits than it could be.
//!
//! ## Deliberately conservative
//!
//! Whisper's log-mel front-end is largely gain-invariant, so this is *not* a
//! large win for whisper-family models — it matters most for the ONNX models and
//! for genuinely quiet input on any model. Because the upside is modest, the
//! design refuses to risk making anything worse:
//!
//! * **Boost only, never attenuate.** Audio already at or above the target is
//!   returned untouched. A loud, healthy signal is never "corrected".
//! * **Bounded gain.** At most [`MAX_GAIN_DB`], so near-silence is amplified
//!   into speech rather than into a wall of hiss.
//! * **Measured over speech, not silence.** RMS is computed from the loudest
//!   portion of the recording, so a long pause with one cough in it cannot drag
//!   the measurement down and cause a huge boost.
//! * **Peak-limited.** A soft limiter runs after the gain so nothing clips, even
//!   when a transient sits well above the RMS.
//! * **Fails open.** Empty, silent, or non-finite input is returned unchanged.

/// Target RMS level in dBFS.
///
/// −20 dBFS is a conventional speech target: comfortably above the noise floor
/// while leaving ~20 dB of headroom for transients.
const TARGET_RMS_DBFS: f32 = -20.0;

/// Maximum boost applied, in dB. Beyond this the input is not quiet speech, it
/// is silence, and amplifying it only raises the noise floor.
const MAX_GAIN_DB: f32 = 20.0;

/// Ceiling for the soft limiter. Slightly below full scale so the subsequent
/// f32→i16 conversion in the WAV writer cannot wrap.
const PEAK_CEILING: f32 = 0.98;

/// Fraction of the loudest frames used to measure speech level.
///
/// Recording usually starts and ends with silence, and the VAD does not always
/// trim it (it can be disabled entirely). Measuring over the loudest 50% of
/// frames approximates "level while actually speaking" without needing to know
/// where the speech is.
const LOUD_FRACTION: f32 = 0.5;

/// Frame size in samples for the loudness measurement (20 ms at 16 kHz).
const FRAME: usize = 320;

/// Minimum mean amplitude before DC removal is considered worthwhile.
///
/// Deliberately not "any non-zero mean". Real audio never averages to exactly
/// zero — a sine wave that does not end on a period boundary has a small
/// residual mean, and speech has more. Subtracting those changes every sample
/// for no benefit and would mean this function almost never returns its input
/// untouched. 0.5% of full scale is well below any offset that actually costs
/// headroom and well above the noise of an honest signal.
const DC_OFFSET_THRESHOLD: f32 = 0.005;

fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// RMS of the loudest [`LOUD_FRACTION`] of frames, in linear scale.
///
/// Returns `None` when there is nothing meaningful to measure.
fn speech_rms(samples: &[f32]) -> Option<f32> {
    if samples.len() < FRAME {
        return None;
    }

    let mut frame_rms: Vec<f32> = samples
        .chunks(FRAME)
        .filter(|chunk| chunk.len() == FRAME)
        .map(|chunk| {
            let sum_sq: f32 = chunk.iter().map(|s| s * s).sum();
            (sum_sq / FRAME as f32).sqrt()
        })
        .filter(|rms| rms.is_finite())
        .collect();

    if frame_rms.is_empty() {
        return None;
    }

    // Descending, so the loudest frames come first.
    frame_rms.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let take = ((frame_rms.len() as f32 * LOUD_FRACTION).ceil() as usize).max(1);

    let mean_sq: f32 = frame_rms[..take].iter().map(|r| r * r).sum::<f32>() / take as f32;
    let rms = mean_sq.sqrt();

    if rms > 0.0 && rms.is_finite() {
        Some(rms)
    } else {
        None
    }
}

/// Remove any DC offset, then bring quiet audio up toward the target level.
///
/// Returns the samples unchanged whenever normalisation would be unsafe or
/// pointless. Never attenuates.
pub fn normalize_for_transcription(mut samples: Vec<f32>) -> Vec<f32> {
    // Anything shorter than one measurement frame is a fragment we cannot
    // reason about — neither its mean nor its RMS means anything. Checked before
    // the DC step so a handful of samples is never "corrected" on the strength
    // of a meaningless statistic.
    if samples.len() < FRAME {
        return samples;
    }

    // --- DC offset -----------------------------------------------------------
    // A constant bias wastes headroom and shifts the waveform off-centre. Some
    // USB interfaces introduce one; it is inaudible but costs dynamic range.
    let mean: f32 = samples.iter().sum::<f32>() / samples.len() as f32;
    if mean.is_finite() && mean.abs() > DC_OFFSET_THRESHOLD {
        for sample in samples.iter_mut() {
            *sample -= mean;
        }
    }

    // --- gain ----------------------------------------------------------------
    let Some(rms) = speech_rms(&samples) else {
        return samples;
    };

    let target = db_to_linear(TARGET_RMS_DBFS);
    if rms >= target {
        // Already loud enough. Leave a healthy signal alone.
        return samples;
    }

    let gain = (target / rms).min(db_to_linear(MAX_GAIN_DB));
    if !gain.is_finite() || gain <= 1.0 {
        return samples;
    }

    for sample in samples.iter_mut() {
        *sample *= gain;
    }

    // --- limiter -------------------------------------------------------------
    // Applied after the gain rather than instead of it: scaling the whole buffer
    // down to fit one transient would undo the boost everywhere else. A single
    // proportional pull-back only engages if something actually exceeds the
    // ceiling, and preserves relative dynamics.
    let peak = samples
        .iter()
        .copied()
        .filter(|s| s.is_finite())
        .fold(0.0f32, |acc, s| acc.max(s.abs()));

    if peak > PEAK_CEILING {
        let scale = PEAK_CEILING / peak;
        for sample in samples.iter_mut() {
            *sample *= scale;
        }
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine wave at a given peak amplitude, one second at 16 kHz.
    fn tone(amplitude: f32) -> Vec<f32> {
        (0..16_000)
            .map(|i| amplitude * (i as f32 * 0.05).sin())
            .collect()
    }

    fn rms_of(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn empty_input_is_returned_unchanged() {
        assert!(normalize_for_transcription(Vec::new()).is_empty());
    }

    #[test]
    fn very_short_input_is_left_alone() {
        // Below one measurement frame there is nothing to measure from — not
        // even a meaningful DC offset, so nothing is touched.
        let short = vec![0.001; 10];
        assert_eq!(normalize_for_transcription(short.clone()), short);
    }

    #[test]
    fn a_tiny_residual_mean_is_not_treated_as_dc_offset() {
        // A sine that does not end on a period boundary has a small non-zero
        // mean. Subtracting it would rewrite every sample of a perfectly good
        // recording for no benefit.
        let healthy = tone(0.5);
        let mean = healthy.iter().sum::<f32>() / healthy.len() as f32;
        assert!(mean.abs() > 0.0, "test needs a non-zero residual mean");
        assert!(
            mean.abs() < DC_OFFSET_THRESHOLD,
            "test premise broken: residual mean {mean} is above the threshold"
        );
        assert_eq!(normalize_for_transcription(healthy.clone()), healthy);
    }

    #[test]
    fn digital_silence_is_left_alone() {
        let silence = vec![0.0f32; 16_000];
        let out = normalize_for_transcription(silence);
        assert!(out.iter().all(|s| *s == 0.0), "silence was amplified");
    }

    #[test]
    fn quiet_audio_is_boosted_toward_the_target() {
        let quiet = tone(0.01); // roughly −43 dBFS RMS
        let before = rms_of(&quiet);
        let after = rms_of(&normalize_for_transcription(quiet));
        assert!(
            after > before * 2.0,
            "expected a real boost: {before} → {after}"
        );
    }

    #[test]
    fn healthy_audio_is_not_attenuated() {
        // A signal already above target must come back untouched — the one
        // behaviour that could damage a well-configured setup.
        let healthy = tone(0.5);
        let out = normalize_for_transcription(healthy.clone());
        assert_eq!(out, healthy);
    }

    #[test]
    fn loud_audio_is_never_touched() {
        let loud = tone(0.95);
        assert_eq!(normalize_for_transcription(loud.clone()), loud);
    }

    #[test]
    fn gain_is_capped_so_near_silence_is_not_blown_up() {
        let almost_silent = tone(1e-6);
        let out = normalize_for_transcription(almost_silent.clone());
        let ratio = rms_of(&out) / rms_of(&almost_silent);
        assert!(
            ratio <= db_to_linear(MAX_GAIN_DB) * 1.01,
            "gain {ratio} exceeded the {MAX_GAIN_DB} dB cap"
        );
    }

    #[test]
    fn output_never_clips() {
        // Quiet body with one loud transient: the boost must not push the
        // transient past full scale.
        let mut samples = tone(0.02);
        samples[8_000] = 0.9;
        samples[8_001] = -0.9;
        let out = normalize_for_transcription(samples);
        let peak = out.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!(peak <= 1.0, "clipped at {peak}");
    }

    #[test]
    fn dc_offset_is_removed() {
        let offset = 0.2;
        let biased: Vec<f32> = tone(0.3).iter().map(|s| s + offset).collect();
        let out = normalize_for_transcription(biased);
        let mean = out.iter().sum::<f32>() / out.len() as f32;
        assert!(mean.abs() < 0.01, "DC offset survived: {mean}");
    }

    #[test]
    fn a_long_silence_with_one_burst_does_not_cause_a_huge_boost() {
        // The failure this guards: measuring RMS over the whole buffer would see
        // near-silence and apply maximum gain, blowing up the burst.
        let mut samples = vec![0.0f32; 16_000];
        for (i, sample) in samples.iter_mut().enumerate().take(3_200) {
            *sample = 0.3 * (i as f32 * 0.05).sin();
        }
        let out = normalize_for_transcription(samples);
        let peak = out.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!(peak <= 1.0, "burst clipped at {peak}");
    }

    #[test]
    fn non_finite_samples_do_not_panic_or_poison_the_buffer() {
        let mut samples = tone(0.01);
        samples[100] = f32::NAN;
        samples[200] = f32::INFINITY;
        // The contract here is only that it returns without panicking; the
        // engine's own handling of non-finite input is out of scope.
        let out = normalize_for_transcription(samples);
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn output_length_always_matches_input_length() {
        for amplitude in [0.001, 0.05, 0.5, 0.99] {
            let input = tone(amplitude);
            let n = input.len();
            assert_eq!(normalize_for_transcription(input).len(), n);
        }
    }
}
