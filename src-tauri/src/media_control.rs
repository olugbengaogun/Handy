//! Pause the user's music for the duration of a recording, and put it back.
//!
//! `mute_while_recording` silences the *output device*, which stops background
//! audio bleeding into the microphone but also means the music plays on,
//! inaudibly, and comes back mid-phrase. Pausing is what a person actually
//! wants: the track stops where it is and resumes from that exact point.
//!
//! Scope is deliberately Spotify on macOS. Spotify exposes a real scripting
//! interface — `player state` can be *queried*, so we can tell "playing" from
//! "already paused" instead of firing a blind play/pause toggle at whatever
//! owns Now Playing and hoping. A toggle that guesses wrong starts music the
//! user never had on, which is a far worse failure than not pausing at all.
//!
//! # Ordering
//!
//! Every AppleScript round trip costs 50–150 ms, which must not sit on the
//! keypress path — dictation starting late is the one thing this app cannot do.
//! So the work happens on a single long-lived worker thread fed by a channel.
//! One thread, not a thread per call: with two threads a short recording can
//! run its resume *before* its own pause lands, which leaves the user's music
//! paused with nothing left to restart it.
//!
//! # Ownership
//!
//! `paused_by` mirrors the ownership discipline `apply_mute` already uses. A
//! second binding firing during a live recording gets a `try_start_recording`
//! failure, and its failure path must not resume the music out from under the
//! recording that is still running — so a resume carrying a token only fires if
//! that token is the one that paused. The end-of-recording paths (stop, cancel,
//! quit) pass no token and resume unconditionally.

#[cfg(target_os = "macos")]
use log::{debug, warn};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Sender, SyncSender};
use std::sync::OnceLock;
use tauri::AppHandle;

/// Handed back by [`pause_for_recording`] and passed to [`resume_owned`].
/// Zero means "this call did not pause anything", so resuming is a no-op.
pub type PauseToken = u64;

pub const NO_PAUSE: PauseToken = 0;

enum Cmd {
    Pause(PauseToken),
    /// `token: None` resumes whatever we paused, whoever paused it.
    Resume {
        token: Option<PauseToken>,
        ack: Option<SyncSender<()>>,
    },
}

static WORKER: OnceLock<Sender<Cmd>> = OnceLock::new();
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn worker() -> &'static Sender<Cmd> {
    WORKER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Cmd>();
        std::thread::Builder::new()
            .name("media-control".into())
            .spawn(move || {
                // Owned by this thread alone, so "did we pause the music?" needs
                // no lock and cannot be read half-updated.
                let mut paused_by: Option<PauseToken> = None;

                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        Cmd::Pause(token) => {
                            // Already ours. Re-pausing would overwrite the
                            // owning token and strand the original resume.
                            if paused_by.is_some() {
                                continue;
                            }
                            if pause_now() {
                                paused_by = Some(token);
                            }
                        }
                        Cmd::Resume { token, ack } => {
                            let ours = match (paused_by, token) {
                                (Some(owner), Some(t)) => owner == t,
                                (Some(_), None) => true,
                                (None, _) => false,
                            };
                            if ours {
                                resume_now();
                                paused_by = None;
                            }
                            if let Some(ack) = ack {
                                // Receiver may have timed out and gone away.
                                let _ = ack.send(());
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn media-control thread");
        tx
    })
}

/// Pauses the user's music if the setting is on. Never blocks the caller.
///
/// The returned token identifies *this* pause. Hand it to [`resume_owned`] on a
/// path that must only undo its own work (a recording that failed to start);
/// end-of-recording paths should call [`resume_any`] instead.
pub fn pause_for_recording(app: &AppHandle) -> PauseToken {
    if !crate::settings::get_settings(app).pause_media_while_recording {
        return NO_PAUSE;
    }
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let _ = worker().send(Cmd::Pause(token));
    token
}

/// Resumes only if `token` is the pause that is currently in effect.
pub fn resume_owned(token: PauseToken) {
    if token == NO_PAUSE {
        return;
    }
    let _ = worker().send(Cmd::Resume {
        token: Some(token),
        ack: None,
    });
}

/// Resumes whatever we paused. For the paths that genuinely end a recording.
pub fn resume_any() {
    // Don't spin up the worker just to tell it to do nothing.
    if let Some(tx) = WORKER.get() {
        let _ = tx.send(Cmd::Resume {
            token: None,
            ack: None,
        });
    }
}

/// Resumes and waits for it to land, for use on the way out of the process.
///
/// Quitting mid-recording must not leave the user's music paused with the only
/// thing that would resume it gone. Bounded, because a wedged Spotify must not
/// be able to hold the app open.
pub fn resume_blocking() {
    let Some(tx) = WORKER.get() else {
        return;
    };
    let (ack_tx, ack_rx) = sync_channel::<()>(1);
    if tx
        .send(Cmd::Resume {
            token: None,
            ack: Some(ack_tx),
        })
        .is_err()
    {
        return;
    }
    let _ = ack_rx.recv_timeout(std::time::Duration::from_secs(3));
}

/* ── platform layer ──────────────────────────────────────────────────────── */

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// `application "X" is running` is answered by Launch Services and does not
    /// start the app — `tell application "Spotify" to pause` on its own would
    /// *launch Spotify*, so pressing the dictation key would open a music
    /// player for someone who had no music on at all.
    const PAUSE_SCRIPT: &str = r#"
with timeout of 2 seconds
	if application "Spotify" is running then
		tell application "Spotify"
			if player state is playing then
				pause
				return "paused"
			end if
		end tell
	end if
end timeout
return "no"
"#;

    /// Guarded on `player state is paused` so we only ever undo our own pause.
    /// If the user hit play themselves during the take, the state is `playing`
    /// and we leave it alone; if they stopped Spotify outright, it is `stopped`
    /// and we do not resurrect music they deliberately silenced.
    const RESUME_SCRIPT: &str = r#"
with timeout of 2 seconds
	if application "Spotify" is running then
		tell application "Spotify"
			if player state is paused then play
		end tell
	end if
end timeout
return "ok"
"#;

    /// `get player state` rather than anything simpler, and that is the whole
    /// point. Entering a `tell` block sends nothing; only a command inside it
    /// does. An earlier version wrapped a bare `return "ok"` in the tell, which
    /// AppleScript answers itself without ever contacting Spotify - so the probe
    /// reported success, no consent prompt was raised, and the dialog would have
    /// ambushed the user mid-sentence on their first dictation, which is exactly
    /// what this exists to prevent. Reading a property is a real Apple Event.
    const PROBE_SCRIPT: &str = r#"
with timeout of 2 seconds
	if application "Spotify" is running then
		tell application "Spotify" to get player state
		return "ok"
	end if
end timeout
return "not_running"
"#;

    /// Runs a script, killing it if it overruns.
    ///
    /// The AppleScript `with timeout` above covers a Spotify that stops
    /// answering Apple Events, which is the realistic hang. This covers
    /// osascript itself wedging: without it the single worker thread would
    /// block forever, and every later resume would queue behind it — the
    /// user's music paused permanently by a stuck helper process.
    fn run(script: &str) -> Result<String, String> {
        const LIMIT: Duration = Duration::from_secs(5);

        let mut child = Command::new("osascript")
            .args(["-e", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run osascript: {e}"))?;

        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if started.elapsed() > LIMIT {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("osascript timed out".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(format!("osascript wait failed: {e}")),
            }
        }

        let mut out = String::new();
        let mut err = String::new();
        if let Some(mut s) = child.stdout.take() {
            let _ = s.read_to_string(&mut out);
        }
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut err);
        }

        if err.trim().is_empty() {
            Ok(out.trim().to_string())
        } else {
            Err(err.trim().to_string())
        }
    }

    /// True only if this call is what stopped the music.
    pub(super) fn pause_now() -> bool {
        match run(PAUSE_SCRIPT) {
            Ok(out) => {
                let paused = out == "paused";
                debug!("media: pause -> {out}");
                paused
            }
            Err(e) => {
                warn!("media: pause failed: {e}");
                false
            }
        }
    }

    pub(super) fn resume_now() {
        match run(RESUME_SCRIPT) {
            Ok(_) => debug!("media: resumed"),
            Err(e) => warn!("media: resume failed: {e}"),
        }
    }

    /// `"ok"` | `"not_running"` | `"denied"` | `"error: …"`.
    pub(super) fn probe_now() -> String {
        match run(PROBE_SCRIPT) {
            Ok(out) => out,
            Err(e) => {
                // -1743 is errAEEventNotPermitted: the user said No, or has
                // never been asked because consent was revoked in Settings.
                if e.contains("-1743") || e.contains("Not authorized") {
                    "denied".to_string()
                } else {
                    format!("error: {e}")
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn pause_now() -> bool {
        false
    }
    pub(super) fn resume_now() {}
    pub(super) fn probe_now() -> String {
        "unsupported".to_string()
    }
}

use platform::{pause_now, probe_now, resume_now};

/// Asks Spotify a harmless question, so the Automation permission prompt lands
/// while the user is looking at the setting rather than mid-dictation.
///
/// Returns a status slug rather than a typed enum on purpose: a new specta enum
/// means a new type in the generated bindings, and `bindings.ts` is one of the
/// files the upstream sync already collides on. One extra function is a far
/// smaller thing to resolve than a new exported type.
#[tauri::command]
#[specta::specta]
pub async fn probe_media_control() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(probe_now)
        .await
        .map_err(|e| format!("probe failed: {e}"))
}

/// Toggles the setting, and resumes immediately if it is switched off while a
/// recording holds the music paused — otherwise turning the feature off during
/// a take would leave the track stopped for good.
#[tauri::command]
#[specta::specta]
pub fn change_pause_media_while_recording_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.pause_media_while_recording = enabled;
    crate::settings::write_settings(&app, settings);
    if !enabled {
        resume_any();
    }
    Ok(())
}
