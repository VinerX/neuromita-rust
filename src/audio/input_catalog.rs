//! Best-effort read-only input discovery; this does not open a microphone.

use std::{path::Path, process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInputDevice {
    pub id: String,
    pub name: String,
    pub backend: String,
}

pub struct AudioInputCatalog;

impl AudioInputCatalog {
    pub async fn discover(ffmpeg: Option<&Path>) -> Vec<AudioInputDevice> {
        #[cfg(target_os = "linux")]
        {
            let output = Self::run("pactl", &["list", "short", "sources"]).await;
            if let Some(output) = output {
                return parse_pulse_sources(&output);
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(exe) = ffmpeg.filter(|path| path.is_absolute() && path.is_file())
            && let Some(output) = Self::run(
                exe.to_string_lossy().as_ref(),
                &[
                    "-hide_banner",
                    "-list_devices",
                    "true",
                    "-f",
                    "dshow",
                    "-i",
                    "dummy",
                ],
            )
            .await
        {
            return parse_dshow_devices(&output);
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let _ = ffmpeg;
        #[cfg(target_os = "linux")]
        let _ = ffmpeg;
        Vec::new()
    }

    async fn run(binary: &str, args: &[&str]) -> Option<String> {
        let child = Command::new(binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let output = timeout(Duration::from_secs(3), child.wait_with_output())
            .await
            .ok()?
            .ok()?;
        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (text.len() <= 512 * 1024).then_some(text)
    }
}

#[cfg(target_os = "linux")]
fn parse_pulse_sources(text: &str) -> Vec<AudioInputDevice> {
    text.lines()
        .take(256)
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            let id = fields.get(1)?.trim();
            if id.is_empty() || id.contains(".monitor") || id.len() > 512 {
                return None;
            }
            Some(AudioInputDevice {
                id: id.into(),
                name: id.into(),
                backend: "pulse".into(),
            })
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn parse_dshow_devices(text: &str) -> Vec<AudioInputDevice> {
    let mut audio = false;
    text.lines()
        .take(512)
        .filter_map(|line| {
            if line.contains("DirectShow audio devices") {
                audio = true;
                return None;
            }
            if line.contains("DirectShow video devices") {
                audio = false;
                return None;
            }
            if !audio || line.contains("Alternative name") {
                return None;
            }
            let text = line.rsplit_once(']')?.1.trim();
            let name = text.strip_prefix('"')?.split('"').next()?.trim();
            if name.is_empty() || name.len() > 512 {
                return None;
            }
            Some(AudioInputDevice {
                id: format!("audio={name}"),
                name: name.into(),
                backend: "dshow".into(),
            })
        })
        .collect()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_excludes_loopback_monitors() {
        let result = parse_pulse_sources(
            "0\talsa_input.usb\tmodule-alsa-card.c\n1\talsa_output.monitor\tmodule-alsa-card.c",
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "alsa_input.usb");
    }
}
