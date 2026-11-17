//! Optional integration with HeadsetControl (https://github.com/Sapd/HeadsetControl), which
//! supports many headsets (SteelSeries, HyperX, Corsair, Logitech, Audeze…).
//!
//! Runs only when found next to our executable or on PATH.

use super::{Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use crate::diag;
use serde::Deserialize;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const EXE_NAMES: &[&str] = &["headsetcontrol.exe", "headsetcontrol-windows-x86_64.exe"];
const TIMEOUT: Duration = Duration::from_secs(30);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    version: String,
    #[serde(default)]
    devices: Vec<Device>,
}

#[derive(Deserialize)]
struct Device {
    #[serde(default)]
    device: String,
    #[serde(default)]
    product: String,
    #[serde(default)]
    id_vendor: String,
    #[serde(default)]
    id_product: String,
    battery: Option<Battery>,
}

#[derive(Deserialize)]
struct Battery {
    #[serde(default)]
    status: String,
    #[serde(default = "unknown_level")]
    level: i32,
}

fn unknown_level() -> i32 {
    -1
}

/// Locates the HeadsetControl executable, next to our exe or on PATH.
pub fn find() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let path_dirs = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    let exe = beside
        .into_iter()
        .chain(path_dirs)
        .flat_map(|dir| EXE_NAMES.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file());
    // Not the path: it usually contains the user name.
    diag::event("headsetcontrol", if exe.is_some() { "found" } else { "not installed (optional)" });
    exe
}

#[derive(Default)]
struct HeadsetControl {
    exe: Option<Option<PathBuf>>,
    cached: Vec<(u16, u16, Observation)>,
    updated: Option<Instant>,
    worker: Option<std::thread::JoinHandle<Vec<(u16, u16, Observation)>>>,
}

pub fn driver() -> Box<dyn Driver> {
    Box::new(HeadsetControl::default())
}

impl Driver for HeadsetControl {
    fn name(&self) -> &'static str {
        "headsetcontrol"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        let Some(exe) = self.exe.get_or_insert_with(find).as_ref() else { return };
        if self.updated.is_none() {
            self.cached = refresh(exe);
            self.updated = Some(Instant::now());
        } else {
            if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
                if let Some(worker) = self.worker.take() {
                    self.cached = worker.join().unwrap_or_default();
                    self.updated = Some(Instant::now());
                }
            }
            if self.worker.is_none() && self.updated.is_some_and(|at| at.elapsed() > Duration::from_secs(50)) {
                let exe = exe.clone();
                self.worker =
                    std::thread::Builder::new().name("headsetcontrol".into()).spawn(move || refresh(&exe)).ok();
            }
        }
        for (vid, pid, obs) in &self.cached {
            if !ctx.is_claimed(*vid, *pid) {
                ctx.add(obs.clone(), *vid, *pid);
            }
        }
    }
}

fn refresh(exe: &Path) -> Vec<(u16, u16, Observation)> {
    match run(exe) {
        Ok(text) => parse_output(&text),
        Err(why) => {
            diag::event("headsetcontrol run", why);
            Vec::new()
        }
    }
}

fn parse_output(text: &str) -> Vec<(u16, u16, Observation)> {
    let Some(start) = text.find('{') else { return Vec::new() };
    let Ok(output) = serde_json::from_str::<Output>(&text[start..]) else { return Vec::new() };
    diag::event("headsetcontrol run", format!("version {}, {} device(s)", output.version, output.devices.len()));
    output
        .devices
        .into_iter()
        .filter_map(|d| {
            let (vid, pid) = (parse_hex(&d.id_vendor)?, parse_hex(&d.id_product)?);
            let battery = d.battery?;
            let connected = matches!(battery.status.as_str(), "BATTERY_AVAILABLE" | "BATTERY_CHARGING");
            let state = if !connected {
                State::Off
            } else if battery.level < 0 {
                State::Unreachable
            } else {
                State::Battery {
                    percent: battery.level.clamp(0, 100) as u8,
                    charging: battery.status == "BATTERY_CHARGING",
                }
            };
            let name = if !d.device.is_empty() {
                d.device
            } else if !d.product.is_empty() {
                d.product
            } else {
                "Headset".into()
            };
            let kind = if Kind::from_name(&name) == Kind::Earbuds { Kind::Earbuds } else { Kind::Headset };
            Some((
                vid,
                pid,
                Observation::new(format!("hsc:{vid:04x}:{pid:04x}"), state).name(name).kind(kind).via("HeadsetControl"),
            ))
        })
        .collect()
}

fn run(exe: &Path) -> Result<String, String> {
    let mut command = Command::new(exe);
    command.args(["-b", "-o", "json", "--timeout", "1500"]);
    if std::env::var_os("WB_HSC_TEST_DEVICE").is_some() {
        command.arg("--test-device");
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("failed to start: {e}"))?;
    use std::io::Read;
    let stdout = child.stdout.take().ok_or("stdout unavailable")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(1 << 20).read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + TIMEOUT;
    while child.try_wait().map_err(|e| e.to_string())?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("no answer within {} s", TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.wait();
    let bytes = reader.join().map_err(|_| "stdout reader failed")?.map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|_| "output isn't UTF-8".into())
}

fn parse_hex(s: &str) -> Option<u16> {
    u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headsetcontrol_json() {
        let json = r#"{"name":"HeadsetControl","version":"4.3.0","device_count":2,"devices":[
            {"status":"success","device":"SteelSeries Arctis Nova 7","vendor":"SteelSeries","product":"Arctis Nova 7",
             "id_vendor":"0x1038","id_product":"0x2202","battery":{"status":"BATTERY_CHARGING","level":64}},
            {"status":"success","device":"Logitech PRO X","vendor":"Logitech","product":"PRO X",
             "id_vendor":"0x046d","id_product":"0x0aba","battery":{"status":"BATTERY_AVAILABLE","level":90}}]}"#;
        let output: Output = serde_json::from_str(json).unwrap();
        assert_eq!(output.devices.len(), 2);
        assert_eq!(parse_hex(&output.devices[0].id_product), Some(0x2202));
        assert_eq!(output.devices[0].battery.as_ref().unwrap().level, 64);
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;
    #[test]
    fn status_and_prefixed_output() {
        let text = r#"notice
        {"devices":[
          {"device":"Headset","id_vendor":"0x1038","id_product":"0x2202","battery":{"status":"BATTERY_CHARGING","level":64}},
          {"id_vendor":"046d","id_product":"0aba","battery":{"status":"BATTERY_UNAVAILABLE","level":-1}},
          {"id_vendor":"1234","id_product":"5678","battery":{"status":"BATTERY_AVAILABLE"}}
        ]}"#;
        let result = parse_output(text);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].2.state, State::Battery { percent: 64, charging: true });
        assert_eq!(result[1].2.state, State::Off);
        assert_eq!(result[2].2.state, State::Unreachable);
        assert!(parse_output("invalid").is_empty());
    }
}
