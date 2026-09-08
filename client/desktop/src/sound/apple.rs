//! Native iOS attention audio. The decoder is also tested on macOS.

use objc2::{rc::Retained, AnyThread};
use objc2_avf_audio::AVAudioPlayer;
use objc2_foundation::NSData;

fn create_player(bytes: &[u8]) -> Result<Retained<AVAudioPlayer>, String> {
    let data = NSData::with_bytes(bytes);
    // SAFETY: NSData owns its bytes, and the player remains on this thread.
    unsafe { AVAudioPlayer::initWithData_error(AVAudioPlayer::alloc(), &data) }
        .map_err(|error| format!("could not decode notification sound: {error}"))
}

#[cfg(target_os = "ios")]
pub(super) fn play(bytes: &'static [u8]) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    // AVAudioPlayer must stay alive after /api/notify/sound returns. Keep it on
    // one worker throughout playback; only the startup result crosses threads.
    let (started, result) = std::sync::mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let timeout = Duration::from_secs(3);
    let deadline = Instant::now() + timeout;
    std::thread::Builder::new()
        .name("notification-sound".into())
        .spawn(move || {
            objc2::rc::autoreleasepool(|_| {
                if let Err(error) = play_on_worker(bytes, &started, &worker_cancelled, deadline) {
                    log::warn!("notification sound failed: {error}");
                    let _ = started.send(Err(error));
                }
            });
        })
        .map_err(|error| format!("could not start notification sound: {error}"))?;
    match result.recv_timeout(timeout) {
        Ok(result) => result,
        Err(error) => {
            // An audio-session activation can block inside iOS. Do not let it
            // occupy a server worker indefinitely, or play after it times out.
            cancelled.store(true, Ordering::Release);
            Err(format!("notification sound did not start in time: {error}"))
        }
    }
}

#[cfg(target_os = "ios")]
fn play_on_worker(
    bytes: &[u8],
    started: &std::sync::mpsc::SyncSender<Result<(), String>>,
    cancelled: &std::sync::atomic::AtomicBool,
    startup_deadline: std::time::Instant,
) -> Result<(), String> {
    use objc2_avf_audio::{AVAudioSession, AVAudioSessionCategoryAmbient};
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, TryLockError};
    use std::time::{Duration, Instant};

    // The session is process-wide. Coalesce while an alert is playing rather
    // than queueing stale sounds or making HTTP workers wait for earlier ones.
    static AUDIO_SESSION: Mutex<()> = Mutex::new(());
    let _session_guard = match AUDIO_SESSION.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => {
            let _ = started.send(Ok(()));
            return Ok(());
        }
        Err(TryLockError::Poisoned(_)) => {
            return Err("notification audio session lock was poisoned".to_string());
        }
    };
    let expired = || cancelled.load(Ordering::Acquire) || Instant::now() >= startup_deadline;
    if expired() {
        return Err("notification sound startup expired".to_string());
    }
    let player = create_player(bytes)?;

    // SAFETY: all access to this player is on its owning worker. Changes to the
    // shared audio session are serialized, and it has no other native users.
    unsafe {
        let session = AVAudioSession::sharedInstance();
        let category = AVAudioSessionCategoryAmbient
            .ok_or_else(|| "ambient audio category is unavailable".to_string())?;
        // Ambient respects the phone's Silent switch and mixes with existing
        // music. Attention sounds should not take over another app's audio.
        session
            .setCategory_error(category)
            .map_err(|error| format!("could not configure notification audio: {error}"))?;
        session
            .setActive_error(true)
            .map_err(|error| format!("could not activate notification audio: {error}"))?;

        let outcome = if expired() {
            Err("notification sound startup expired".to_string())
        } else if !player.play() {
            Err("AVAudioPlayer could not start notification sound".to_string())
        } else if expired() || started.send(Ok(())).is_err() {
            // If cancellation races a synchronous native play call, stop as
            // soon as it returns instead of keeping an expired alert alive.
            Err("notification sound startup expired".to_string())
        } else {
            let deadline = Instant::now() + Duration::from_secs(15);
            while player.isPlaying()
                && Instant::now() < deadline
                && !cancelled.load(Ordering::Acquire)
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            if player.isPlaying() {
                log::warn!("notification sound exceeded playback timeout");
            }
            Ok(())
        };
        player.stop();
        if let Err(error) = session.setActive_error(false) {
            log::warn!("could not deactivate notification audio: {error}");
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_decoder_accepts_both_embedded_attention_sounds() {
        objc2::rc::autoreleasepool(|_| {
            for bytes in [super::super::SOUND_DONE, super::super::SOUND_REQUEST] {
                let player = create_player(bytes).expect("bundled MP3 should decode natively");
                // SAFETY: this test owns the player and does not start playback.
                unsafe {
                    assert!(player.duration() > 0.0 && player.duration() < 15.0);
                    assert!(player.numberOfChannels() > 0);
                    assert!(!player.isPlaying());
                }
            }
        });
    }

    #[test]
    fn native_decoder_returns_an_error_for_invalid_audio() {
        objc2::rc::autoreleasepool(|_| {
            assert!(create_player(b"not an MP3").is_err());
        });
    }
}
