//! Sound notifications for agent state changes.
//!
//! Embeds MP3 files in the binary. iOS uses AVAudioPlayer; desktops use
//! afplay (macOS), Windows MediaPlayer, or decoder-capable Linux audio tools.
//!
//! Adapted from Herdr (Apache-2.0), commit
//! 4b5e9bda239a0b6903889062d756424578e94691, src/sound.rs.
//! See server/skill-core/src/agent_detection/NOTICE.txt for attribution and license.

#[cfg(not(target_os = "ios"))]
use std::io::Write;
#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
use std::io::{Read, Result as IoResult};
#[cfg(not(target_os = "ios"))]
use std::path::{Path, PathBuf};
#[cfg(any(windows, all(test, not(target_os = "ios"))))]
use std::process::Command;
#[cfg(not(target_os = "ios"))]
use std::process::Output;
#[cfg(not(target_os = "ios"))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
use std::time::{Duration, Instant};

#[cfg(not(target_os = "ios"))]
use log::warn;
#[cfg(not(target_os = "ios"))]
use skill_core::process::hidden_command;
use skill_server::NotificationSound as Sound;

#[cfg(any(target_os = "ios", all(test, target_os = "macos")))]
mod apple;

const DISABLE_SOUND_ENV: &str = "VIBESTUDIO_DISABLE_SOUND";
#[cfg(any(windows, all(test, not(target_os = "ios"))))]
const WINDOWS_SOUND_PATH_ENV: &str = "VIBESTUDIO_SOUND_PATH";
#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
const AUDIO_PLAYER_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
const AUDIO_PLAYER_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[cfg(not(target_os = "ios"))]
static SOUND_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static SOUND_DONE: &[u8] = include_bytes!("../../../public/sounds/done.mp3");
static SOUND_REQUEST: &[u8] = include_bytes!("../../../public/sounds/request.mp3");

/// Play a notification sound, retaining the native player until it finishes.
/// On iOS, report startup failures instead of claiming playback was handled.
pub fn play(sound: Sound) -> Result<(), String> {
    if sound_playback_disabled_by_env() {
        return Ok(());
    }

    let data = match sound {
        Sound::Done => SOUND_DONE,
        Sound::Request => SOUND_REQUEST,
    };

    #[cfg(target_os = "ios")]
    return apple::play(data);

    #[cfg(not(target_os = "ios"))]
    {
        std::thread::spawn(move || {
            if let Err(err) = play_bytes(data) {
                warn!("{sound:?} sound playback failed: {err}");
            }
        });
        Ok(())
    }
}

fn sound_playback_disabled_by_env() -> bool {
    std::env::var_os(DISABLE_SOUND_ENV).is_some() || std::env::var_os("NEXTEST").is_some()
}

#[cfg(not(target_os = "ios"))]
fn play_bytes(data: &[u8]) -> Result<(), String> {
    // Write to a temp file because the supported audio players need a file path.
    let tmp = temp_sound_path();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    if let Err(e) = file.write_all(data) {
        drop(file);
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    drop(file);

    let result = run_player(&tmp);

    let _ = std::fs::remove_file(&tmp);

    match result {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(playback_error(&output)),
        Err(e) => Err(e),
    }
}

#[cfg(not(target_os = "ios"))]
fn playback_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("player exited with {}", output.status)
    } else {
        format!("player exited with {}: {stderr}", output.status)
    }
}

#[cfg(not(target_os = "ios"))]
fn temp_sound_path() -> PathBuf {
    let id = SOUND_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("vibestudio-sound-{}-{id}.mp3", std::process::id()))
}

#[cfg(windows)]
fn run_player(path: &Path) -> Result<Output, String> {
    run_windows_player(path)
}

#[cfg(target_os = "macos")]
fn run_player(path: &Path) -> Result<Output, String> {
    hidden_command("afplay")
        .arg(path)
        .output()
        .map_err(|e| format!("no audio player available: {e}"))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
fn run_player(path: &Path) -> Result<Output, String> {
    run_linux_player(path)
}

#[cfg(any(windows, all(test, not(target_os = "ios"))))]
fn windows_media_player_script() -> &'static str {
    r#"
$ErrorActionPreference = 'Stop'
$Path = [Environment]::GetEnvironmentVariable('VIBESTUDIO_SOUND_PATH', 'Process')
if ([string]::IsNullOrWhiteSpace($Path)) { throw 'VIBESTUDIO_SOUND_PATH is not set' }
Add-Type -AssemblyName PresentationCore
Add-Type -AssemblyName WindowsBase
$resolved = (Resolve-Path -LiteralPath $Path).ProviderPath
$script:player = [System.Windows.Media.MediaPlayer]::new()
$script:frame = [System.Windows.Threading.DispatcherFrame]::new()
$script:timer = [System.Windows.Threading.DispatcherTimer]::new()
$script:timer.Interval = [TimeSpan]::FromSeconds(15)
$script:failed = $null
$script:timedOut = $false
$script:player.add_MediaOpened({ $script:player.Play() })
$script:player.add_MediaEnded({ $script:frame.Continue = $false })
$script:player.add_MediaFailed({
    param($sender, $eventArgs)
    $script:failed = $eventArgs.ErrorException
    $script:frame.Continue = $false
})
$script:timer.add_Tick({
    $script:timedOut = $true
    $script:frame.Continue = $false
})
try {
    $script:player.Open([Uri]::new($resolved))
    $script:timer.Start()
    [System.Windows.Threading.Dispatcher]::PushFrame($script:frame)
} finally {
    $script:timer.Stop()
    $script:player.Close()
}
if ($script:failed) { throw "sound media failed: $($script:failed.Message)" }
if ($script:timedOut) { throw 'sound playback timed out' }
"#
}

#[cfg(any(windows, all(test, not(target_os = "ios"))))]
fn windows_player_command(path: &Path) -> Command {
    let mut command = hidden_command("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            windows_media_player_script(),
        ])
        .env(WINDOWS_SOUND_PATH_ENV, path);
    command
}

#[cfg(windows)]
fn run_windows_player(path: &Path) -> Result<Output, String> {
    windows_player_command(path)
        .output()
        .map_err(|e| format!("Windows MediaPlayer playback failed: {e}"))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
#[derive(Debug, Clone, Copy)]
struct AudioPlayer {
    program: &'static str,
    args: &'static [&'static str],
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
impl AudioPlayer {
    fn output(self, path: &Path) -> std::io::Result<Output> {
        self.output_with_timeout(path, AUDIO_PLAYER_TIMEOUT)
    }

    fn output_with_timeout(self, path: &Path, timeout: Duration) -> std::io::Result<Output> {
        let mut child = hidden_command(self.program)
            .args(self.args)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let Some(stdout) = child.stdout.take() else {
            terminate_and_reap(&mut child)?;
            return Err(std::io::Error::other("audio player stdout was not piped"));
        };
        let Some(stderr) = child.stderr.take() else {
            terminate_and_reap(&mut child)?;
            return Err(std::io::Error::other("audio player stderr was not piped"));
        };
        let stdout_reader = read_output(stdout);
        let stderr_reader = read_output(stderr);
        let deadline = Instant::now() + timeout;

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let (stdout, stderr) = finish_output(stdout_reader, stderr_reader)?;
                    return Ok(Output {
                        status,
                        stdout,
                        stderr,
                    });
                }
                Ok(None) => {}
                Err(wait_err) => {
                    let cleanup_result = terminate_and_reap(&mut child);
                    let _ = finish_output(stdout_reader, stderr_reader);
                    cleanup_result?;
                    return Err(wait_err);
                }
            }

            let now = Instant::now();
            if now >= deadline {
                let cleanup_result = terminate_and_reap(&mut child);
                let _ = finish_output(stdout_reader, stderr_reader);
                cleanup_result?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{} playback timed out after {timeout:?}", self.program),
                ));
            }

            std::thread::sleep((deadline - now).min(AUDIO_PLAYER_POLL_INTERVAL));
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
fn read_output<R>(mut reader: R) -> std::thread::JoinHandle<IoResult<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output)?;
        Ok(output)
    })
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
fn finish_output(
    stdout_reader: std::thread::JoinHandle<IoResult<Vec<u8>>>,
    stderr_reader: std::thread::JoinHandle<IoResult<Vec<u8>>>,
) -> IoResult<(Vec<u8>, Vec<u8>)> {
    let stdout = stdout_reader
        .join()
        .map_err(|_| std::io::Error::other("audio player stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| std::io::Error::other("audio player stderr reader panicked"))??;
    Ok((stdout, stderr))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
fn terminate_and_reap(child: &mut std::process::Child) -> std::io::Result<()> {
    if let Err(kill_err) = child.kill() {
        if child.try_wait()?.is_none() {
            return Err(kill_err);
        }
    }
    child.wait().map(|_| ())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
fn linux_audio_players() -> &'static [AudioPlayer] {
    // Do not add bare aplay here. It does not decode MP3 and plays MP3 bytes as raw PCM.
    &[
        AudioPlayer {
            program: "paplay",
            args: &[],
        },
        AudioPlayer {
            program: "pw-play",
            args: &[],
        },
        AudioPlayer {
            program: "ffplay",
            args: &["-nodisp", "-autoexit", "-loglevel", "quiet"],
        },
        AudioPlayer {
            program: "mpg123",
            args: &["-q"],
        },
        AudioPlayer {
            program: "mpv",
            args: &["--no-video", "--really-quiet"],
        },
    ]
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
fn run_linux_player(path: &Path) -> Result<Output, String> {
    let mut errors = Vec::new();

    for player in linux_audio_players() {
        match player.output(path) {
            Ok(output) if output.status.success() => return Ok(output),
            Ok(output) => errors.push(player_error(*player, &output)),
            Err(err) => errors.push(format!("{} failed: {err}", player.program)),
        }
    }

    Err(format!(
        "no mp3-capable audio player available: {}",
        errors.join("; ")
    ))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
fn player_error(player: AudioPlayer, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();

    if stderr.is_empty() {
        format!("{} exited with {}", player.program, output.status)
    } else {
        format!("{} exited with {}: {stderr}", player.program, output.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "ios"))]
    #[test]
    fn temp_sound_paths_are_unique() {
        assert_ne!(temp_sound_path(), temp_sound_path());
    }

    #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
    #[test]
    fn linux_audio_players_are_mp3_capable() {
        let programs: Vec<&str> = linux_audio_players()
            .iter()
            .map(|player| player.program)
            .collect();

        assert_eq!(programs, ["paplay", "pw-play", "ffplay", "mpg123", "mpv"]);
        assert!(!programs.contains(&"aplay"));
    }

    #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
    #[test]
    fn linux_audio_player_does_not_wait_forever() {
        let pid_path = temp_sound_path().with_extension("pid");
        let player = AudioPlayer {
            program: "sh",
            args: &[
                "-c",
                "printf '%s' \"$$\" > \"$1\"; exec sleep 2",
                "herdr-sound-timeout-test",
            ],
        };
        let result = player.output_with_timeout(&pid_path, Duration::from_millis(100));
        let pid = std::fs::read_to_string(&pid_path)
            .expect("hanging test player should record its process ID");
        let _ = std::fs::remove_file(pid_path);

        let err = result.expect_err("hanging audio player should time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        let status = hidden_command("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("test should inspect the timed-out player PID");
        assert!(
            !status.success(),
            "timed-out audio player should be terminated and reaped"
        );
    }

    #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
    #[test]
    fn linux_audio_player_preserves_completed_output() {
        let player = AudioPlayer {
            program: "sh",
            args: &[
                "-c",
                "i=0; while [ \"$i\" -lt 8192 ]; do printf 0123456789abcdef; i=$((i + 1)); done; i=0; while [ \"$i\" -lt 8192 ]; do printf fedcba9876543210; i=$((i + 1)); done >&2; exit 7",
                "herdr-sound-output-test",
            ],
        };

        let output = player
            .output_with_timeout(Path::new("unused.mp3"), Duration::from_secs(5))
            .expect("completed audio player should return its output");

        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout.len(), 131_072);
        assert_eq!(output.stderr.len(), 131_072);
        assert!(output.stdout.starts_with(b"0123456789abcdef"));
        assert!(output.stderr.starts_with(b"fedcba9876543210"));
    }

    #[cfg(not(target_os = "ios"))]
    #[test]
    fn windows_media_player_uses_process_environment_and_dispatcher() {
        let script = windows_media_player_script();
        let path = Path::new(r"C:\sound dir\döne.mp3");
        let command = windows_player_command(path);
        let env_path = command.get_envs().find_map(|(key, value)| {
            (key == std::ffi::OsStr::new(WINDOWS_SOUND_PATH_ENV))
                .then_some(value)
                .flatten()
        });

        assert!(script.contains("GetEnvironmentVariable('VIBESTUDIO_SOUND_PATH', 'Process')"));
        assert!(!script.contains("param([string]$Path)"));
        assert!(script.contains("Resolve-Path -LiteralPath $Path"));
        assert!(script.contains("Dispatcher]::PushFrame"));
        assert!(script.contains("add_MediaEnded"));
        assert!(script.contains("add_MediaFailed"));
        assert_eq!(env_path, Some(path.as_os_str()));
        assert!(!command.get_args().any(|arg| arg == path.as_os_str()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_media_player_reports_invalid_media_without_waiting_for_timeout() {
        let path = temp_sound_path();
        std::fs::write(&path, b"not an mp3").unwrap();
        let output = run_windows_player(&path).unwrap();
        let _ = std::fs::remove_file(path);

        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("sound media failed"),
            "stderr should identify a MediaFailed error"
        );
    }
}
