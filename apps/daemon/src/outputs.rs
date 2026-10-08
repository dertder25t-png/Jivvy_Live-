//! Which output window goes on which monitor, at what resolution. Pure logic, so it is
//! tested without a display; `jivvy-outputs` turns the plan into real windows.
//!
//! Configuration is `outputs.json` in the data directory. Without it, every monitor except
//! the primary one (the operator's screen) gets an output at its native resolution, so a
//! plugged-in projector just works. An output never falls back to the operator's screen
//! when its monitor is missing: it waits, and comes back when the monitor does.

use std::path::{Path, PathBuf};

use crate::client::LiveState;

use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "outputs.json";
pub const STATUS_FILE: &str = "outputs-status.json";
pub const CONFIG_VERSION: u32 = 1;
/// Largest canvas side accepted; anything else falls back to the monitor's native size.
pub const MAX_SIDE: u32 = 16_384;
const MIN_SIDE: u32 = 16;

/// A monitor as the OS reports it, in physical pixels (per-monitor DPI already applied).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorInfo {
    /// OS name, e.g. `\\.\DISPLAY2` on Windows. Stable while the cable stays in the same port.
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputsConfig {
    pub version: u32,
    pub outputs: Vec<OutputConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputConfig {
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// A monitor name from the status file, or `"primary"` to use the operator's screen on
    /// purpose (single-screen setups and testing).
    pub monitor: String,
    /// Canvas size in pixels. Both or neither; omitted means the monitor's native size.
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

/// One window to show.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub id: String,
    pub label: String,
    pub monitor: MonitorInfo,
    /// The slide is laid out at this size, then scaled to fit the monitor (letterboxed).
    pub canvas_width: u32,
    pub canvas_height: u32,
}

/// Something the System screen should show a tech lead.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Problem {
    pub output: String,
    pub message: String,
}

/// What `jivvy-outputs` is doing, written to `outputs-status.json` for the System screen
/// (and the chaos tests) whenever it changes.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Connected to the engine right now. While false, screens keep the last state shown.
    pub connected: bool,
    pub monitors: Vec<MonitorInfo>,
    pub outputs: Vec<Placement>,
    pub problems: Vec<Problem>,
    /// The state on screen.
    pub shown: Option<LiveState>,
}

/// Reads `outputs.json`. Missing means automatic mode (`Ok(None)`); unreadable or invalid
/// is an error, and the caller keeps the windows it already has.
pub fn load_config(dir: &Path) -> Result<Option<OutputsConfig>, String> {
    let bytes = match std::fs::read(dir.join(CONFIG_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("can't read {CONFIG_FILE}: {e}")),
    };
    let config: OutputsConfig =
        serde_json::from_slice(&bytes).map_err(|e| format!("{CONFIG_FILE} is not valid: {e}"))?;
    if config.version != CONFIG_VERSION {
        return Err(format!("{CONFIG_FILE} has version {}, expected {CONFIG_VERSION}", config.version));
    }
    Ok(Some(config))
}

fn canvas(o: &OutputConfig, m: &MonitorInfo, problems: &mut Vec<Problem>) -> (u32, u32) {
    let ok = |v: u32| (MIN_SIDE..=MAX_SIDE).contains(&v);
    match (o.width, o.height) {
        (None, None) => (m.width, m.height),
        (Some(w), Some(h)) if ok(w) && ok(h) => (w, h),
        _ => {
            problems.push(Problem {
                output: o.id.clone(),
                message: format!(
                    "resolution must be both width and height, {MIN_SIDE}–{MAX_SIDE}; using the monitor's {}×{}",
                    m.width, m.height
                ),
            });
            (m.width, m.height)
        }
    }
}

/// Decides the windows to show for this config and these monitors.
pub fn plan(config: Option<&OutputsConfig>, monitors: &[MonitorInfo]) -> (Vec<Placement>, Vec<Problem>) {
    let mut problems = Vec::new();
    let Some(config) = config else {
        let placements = monitors
            .iter()
            .filter(|m| !m.primary)
            .map(|m| Placement {
                id: format!("auto:{}", m.name),
                label: String::new(),
                monitor: m.clone(),
                canvas_width: m.width,
                canvas_height: m.height,
            })
            .collect();
        return (placements, problems);
    };

    let mut placements: Vec<Placement> = Vec::new();
    for o in config.outputs.iter().filter(|o| o.enabled) {
        if placements.iter().any(|p| p.id == o.id) {
            problems.push(Problem { output: o.id.clone(), message: "another output already uses this id".into() });
            continue;
        }
        let monitor = if o.monitor == "primary" {
            monitors.iter().find(|m| m.primary)
        } else {
            monitors.iter().find(|m| m.name == o.monitor)
        };
        let Some(m) = monitor else {
            problems.push(Problem {
                output: o.id.clone(),
                message: format!("monitor {} is not connected; the output will appear when it is", o.monitor),
            });
            continue;
        };
        if let Some(other) = placements.iter().find(|p| p.monitor.name == m.name) {
            problems.push(Problem {
                output: o.id.clone(),
                message: format!("monitor {} is already used by output {}", m.name, other.id),
            });
            continue;
        }
        let (canvas_width, canvas_height) = canvas(o, m, &mut problems);
        placements.push(Placement {
            id: o.id.clone(),
            label: o.label.clone(),
            monitor: m.clone(),
            canvas_width,
            canvas_height,
        });
    }
    (placements, problems)
}

/// Everything `jivvy-outputs` knows, minus the windows themselves.
pub struct Model {
    dir: PathBuf,
    /// Last config that loaded; kept when the file later becomes invalid.
    config: Option<OutputsConfig>,
    config_error: Option<String>,
    pub monitors: Vec<MonitorInfo>,
    pub placements: Vec<Placement>,
    pub problems: Vec<Problem>,
    pub connected: bool,
    pub shown: Option<LiveState>,
    last_written: Vec<u8>,
}

impl Model {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Model {
            dir: dir.into(),
            config: None,
            config_error: None,
            monitors: Vec::new(),
            placements: Vec::new(),
            problems: Vec::new(),
            connected: false,
            shown: None,
            last_written: Vec::new(),
        }
    }

    /// Re-reads the config and re-plans for these monitors. An invalid config file keeps
    /// the last good one (a typo must never blank the projector) and is reported.
    pub fn refresh(&mut self, monitors: Vec<MonitorInfo>) {
        match load_config(&self.dir) {
            Ok(c) => (self.config, self.config_error) = (c, None),
            Err(e) => self.config_error = Some(e),
        }
        let (placements, mut problems) = plan(self.config.as_ref(), &monitors);
        if let Some(e) = &self.config_error {
            problems.insert(0, Problem { output: String::new(), message: format!("{e}; keeping the last good setup") });
        }
        (self.monitors, self.placements, self.problems) = (monitors, placements, problems);
    }

    pub fn status(&self) -> Status {
        Status {
            connected: self.connected,
            monitors: self.monitors.clone(),
            outputs: self.placements.clone(),
            problems: self.problems.clone(),
            shown: self.shown,
        }
    }

    /// Writes `outputs-status.json` if anything changed since the last write.
    pub fn write_status(&mut self) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.status())?;
        if bytes != self.last_written {
            crate::write_atomic(&self.dir.join(STATUS_FILE), &bytes)?;
            self.last_written = bytes;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_keeps_the_last_good_config_when_the_file_breaks_and_writes_status_on_change() {
        let dir = std::env::temp_dir().join(format!("jivvy-outputs-model-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let monitors = vec![mon("D1", 0, true), mon("D2", 1920, false)];
        let mut m = Model::new(&dir);
        std::fs::write(dir.join(CONFIG_FILE), r#"{"version":1,"outputs":[{"id":"p","monitor":"primary"}]}"#).unwrap();
        m.refresh(monitors.clone());
        assert_eq!(m.placements[0].monitor.name, "D1");
        std::fs::write(dir.join(CONFIG_FILE), "{broken").unwrap();
        m.refresh(monitors.clone());
        assert_eq!(m.placements[0].monitor.name, "D1", "a broken file keeps the screens as they were");
        assert!(m.problems[0].message.contains("keeping the last good setup"));

        m.write_status().unwrap();
        let written = std::fs::metadata(dir.join(STATUS_FILE)).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        m.write_status().unwrap();
        assert_eq!(std::fs::metadata(dir.join(STATUS_FILE)).unwrap().modified().unwrap(), written);
        m.shown = Some(LiveState { slide_index: 2, black: false, stream_wanted: false });
        m.write_status().unwrap();
        let status: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(STATUS_FILE)).unwrap()).unwrap();
        assert_eq!(status["shown"]["slideIndex"], 2);
    }

    fn mon(name: &str, x: i32, primary: bool) -> MonitorInfo {
        MonitorInfo { name: name.into(), x, y: 0, width: 1920, height: 1080, scale: 1.0, primary }
    }

    fn out(id: &str, monitor: &str) -> OutputConfig {
        OutputConfig {
            id: id.into(),
            label: String::new(),
            monitor: monitor.into(),
            width: None,
            height: None,
            enabled: true,
        }
    }

    fn cfg(outputs: Vec<OutputConfig>) -> OutputsConfig {
        OutputsConfig { version: CONFIG_VERSION, outputs }
    }

    #[test]
    fn automatic_mode_uses_every_monitor_except_the_operators() {
        let monitors = [mon("D1", 0, true), mon("D2", 1920, false), mon("D3", -1920, false)];
        let (p, problems) = plan(None, &monitors);
        assert_eq!(p.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["auto:D2", "auto:D3"]);
        assert_eq!((p[1].monitor.x, p[1].canvas_width), (-1920, 1920));
        assert!(problems.is_empty());
        assert!(plan(None, &[mon("D1", 0, true)]).0.is_empty());
    }

    #[test]
    fn a_missing_monitor_never_falls_back_to_the_operators_screen() {
        let (p, problems) = plan(Some(&cfg(vec![out("proj", "D2")])), &[mon("D1", 0, true)]);
        assert!(p.is_empty());
        assert!(problems[0].message.contains("not connected"));
    }

    #[test]
    fn primary_only_when_asked_for_by_name() {
        let (p, _) = plan(Some(&cfg(vec![out("test", "primary")])), &[mon("D1", 0, true), mon("D2", 1920, false)]);
        assert_eq!(p[0].monitor.name, "D1");
    }

    #[test]
    fn custom_resolution_is_kept_and_bad_ones_fall_back_to_native() {
        let mut led = out("led", "D2");
        (led.width, led.height) = (Some(1536), Some(512));
        let mut bad = out("bad", "D3");
        (bad.width, bad.height) = (Some(1280), None);
        let mut huge = out("huge", "D4");
        (huge.width, huge.height) = (Some(MAX_SIDE + 1), Some(100));
        let monitors = [mon("D1", 0, true), mon("D2", 1, false), mon("D3", 2, false), mon("D4", 3, false)];
        let (p, problems) = plan(Some(&cfg(vec![led, bad, huge])), &monitors);
        let sizes: Vec<_> = p.iter().map(|p| (p.canvas_width, p.canvas_height)).collect();
        assert_eq!(sizes, [(1536, 512), (1920, 1080), (1920, 1080)]);
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn duplicates_and_disabled_outputs_are_skipped_with_a_reason() {
        let mut off = out("off", "D3");
        off.enabled = false;
        let config = cfg(vec![out("a", "D2"), out("a", "D3"), out("b", "D2"), off]);
        let (p, problems) = plan(Some(&config), &[mon("D1", 0, true), mon("D2", 1, false), mon("D3", 2, false)]);
        assert_eq!(p.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a"]);
        let messages: Vec<_> = problems.iter().map(|p| p.message.as_str()).collect();
        assert!(messages[0].contains("already uses this id") && messages[1].contains("already used by output a"));
    }

    #[test]
    fn config_file_missing_is_automatic_and_invalid_is_an_error() {
        let dir = std::env::temp_dir().join(format!("jivvy-outputs-cfg-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load_config(&dir), Ok(None));
        std::fs::write(dir.join(CONFIG_FILE), r#"{"version":1,"outputs":[{"id":"p","monitor":"D2"}]}"#).unwrap();
        let c = load_config(&dir).unwrap().unwrap();
        assert!(c.outputs[0].enabled && c.outputs[0].width.is_none());
        std::fs::write(dir.join(CONFIG_FILE), "{oops").unwrap();
        assert!(load_config(&dir).unwrap_err().contains("not valid"));
        std::fs::write(dir.join(CONFIG_FILE), r#"{"version":2,"outputs":[]}"#).unwrap();
        assert!(load_config(&dir).unwrap_err().contains("version 2"));
    }
}
