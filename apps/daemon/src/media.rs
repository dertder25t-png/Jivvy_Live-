//! Camera and microphone choice for `jivvy-video`. Pure logic, tested without devices.
//!
//! Configuration is `media.json` in the data directory; without it the first camera and
//! the computer's default microphone are used. A chosen device that is unplugged is
//! waited for, never silently swapped for another one (the stream would suddenly show a
//! different camera); the status file reports it so the tech lead can see why.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "media.json";
pub const STATUS_FILE: &str = "media-status.json";
pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Camera,
    Microphone,
}

/// A device as GStreamer's device monitor reports it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawDevice {
    pub name: String,
    pub class: String,
    pub props: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub kind: Kind,
    /// Stable identity: the device path or id. Two identical USB cameras differ here.
    pub id: String,
    pub name: String,
    pub api: String,
    /// The computer's default microphone.
    pub default: bool,
}

fn prop<'a>(d: &'a RawDevice, key: &str) -> Option<&'a str> {
    d.props.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

fn kind_of(d: &RawDevice) -> Option<Kind> {
    let c = d.class.to_ascii_lowercase();
    if c.contains("video") && c.contains("source") {
        Some(Kind::Camera)
    } else if c.contains("audio") && c.contains("source") {
        Some(Kind::Microphone)
    } else {
        None
    }
}

/// Turns the monitor's raw list into what a tech lead can choose from: speaker loopback
/// "inputs" removed, the Windows "default device" stand-in folded into a `default` flag on
/// the real microphone, and cameras reported twice by different Windows APIs listed once
/// (Media Foundation preferred: it hands frames over without CPU conversion).
pub fn devices(raw: &[RawDevice]) -> Vec<DeviceInfo> {
    let mut out: Vec<DeviceInfo> = Vec::new();
    let default_mic = raw
        .iter()
        .filter(|d| kind_of(d) == Some(Kind::Microphone) && prop(d, "device.default") == Some("true"))
        .find_map(|d| prop(d, "device.actual-id"));
    for d in raw {
        let Some(kind) = kind_of(d) else { continue };
        if prop(d, "wasapi2.device.loopback") == Some("true") {
            continue; // what the speakers play, not a microphone
        }
        // The Windows "default device" stand-in points at a real device listed separately.
        if kind == Kind::Microphone
            && prop(d, "device.actual-id").is_some()
            && prop(d, "device.default") == Some("true")
        {
            continue;
        }
        let id = prop(d, "device.path").or(prop(d, "device.id")).unwrap_or(&d.name).to_string();
        let api = prop(d, "device.api").unwrap_or("").to_string();
        let default = kind == Kind::Microphone && default_mic == Some(id.as_str());
        let info = DeviceInfo { kind, id, name: d.name.clone(), api, default };
        // The same device: same id, or same name seen through a different Windows API.
        // (Two identical cameras on the same API keep different ids, so both stay.)
        let same = out.iter().position(|o| {
            o.kind == kind && (o.id.eq_ignore_ascii_case(&info.id) || (o.name == info.name && o.api != info.api))
        });
        match same {
            Some(i) if out[i].api != "mediafoundation" && info.api == "mediafoundation" => out[i] = info,
            Some(_) => {}
            None => out.push(info),
        }
    }
    out
}

/// What to use for one input, from `media.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "use", rename_all = "lowercase")]
pub enum Selection {
    /// First camera / the default microphone.
    #[default]
    Auto,
    /// No input (e.g. slides-only churches with no camera).
    None,
    /// A generated test pattern or tone (testing without hardware).
    Test,
    /// A specific device: matched by id, then by name if the id changed.
    Device { id: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MediaConfig {
    pub version: u32,
    #[serde(default)]
    pub camera: Selection,
    #[serde(default)]
    pub microphone: Selection,
}

/// Reads `media.json`; missing means defaults. Invalid is an error and the caller keeps
/// what it is already using.
pub fn load_config(dir: &Path) -> Result<MediaConfig, String> {
    let bytes = match std::fs::read(dir.join(CONFIG_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MediaConfig { version: CONFIG_VERSION, ..Default::default() });
        }
        Err(e) => return Err(format!("can't read {CONFIG_FILE}: {e}")),
    };
    let c: MediaConfig = serde_json::from_slice(&bytes).map_err(|e| format!("{CONFIG_FILE} is not valid: {e}"))?;
    if c.version != CONFIG_VERSION {
        return Err(format!("{CONFIG_FILE} has version {}, expected {CONFIG_VERSION}", c.version));
    }
    Ok(c)
}

/// The source an input pipeline should be built from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "source", rename_all = "lowercase")]
pub enum Choice {
    Device {
        id: String,
        name: String,
    },
    Test,
    None,
    /// Configured device not connected; wait for it.
    Missing {
        name: String,
    },
}

pub fn choose(sel: &Selection, kind: Kind, devices: &[DeviceInfo]) -> Choice {
    let of_kind = || devices.iter().filter(move |d| d.kind == kind);
    let pick = |d: &DeviceInfo| Choice::Device { id: d.id.clone(), name: d.name.clone() };
    match sel {
        Selection::None => Choice::None,
        Selection::Test => Choice::Test,
        Selection::Auto => {
            of_kind().find(|d| d.default).or_else(|| of_kind().next()).map(pick).unwrap_or(Choice::Missing {
                name: format!("any {}", if kind == Kind::Camera { "camera" } else { "microphone" }),
            })
        }
        Selection::Device { id, name } => of_kind()
            .find(|d| d.id == *id)
            .or_else(|| of_kind().find(|d| d.name == *name))
            .map(pick)
            .unwrap_or(Choice::Missing { name: name.clone() }),
    }
}

/// Converts a level element reading (dB, can be -inf) into a meter-safe value.
pub fn meter_db(db: f64) -> f64 {
    if db.is_finite() { db.clamp(-100.0, 0.0) } else { -100.0 }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputStatus {
    pub choice: Choice,
    /// "running", "starting", "waiting" (device missing or failed; retried every second) or "off".
    pub state: &'static str,
    pub detail: String,
    /// Frames per second seen (camera) or level reports per second (microphone).
    pub rate: f64,
}

/// `media-status.json`, for the System screen and the chaos tests.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub devices: Vec<DeviceInfo>,
    pub camera: InputStatus,
    pub microphone: InputStatus,
    pub peak_db: Vec<f64>,
    pub connected: bool,
    pub problems: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, class: &str, props: &[(&str, &str)]) -> RawDevice {
        RawDevice {
            name: name.into(),
            class: class.into(),
            props: props.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    /// What this laptop's device monitor reports (Windows, GStreamer 1.28).
    fn laptop() -> Vec<RawDevice> {
        vec![
            raw("Laptop Camera", "Video/Source", &[("device.api", "ksvideo")]),
            raw(
                "Laptop Camera",
                "Source/Video",
                &[("device.api", "mediafoundation"), ("device.path", r"\\?\usb#vid_0bda&pid_5634")],
            ),
            raw(
                "Default Audio Capture Device",
                "Audio/Source",
                &[
                    ("device.api", "wasapi2"),
                    ("device.id", "{2EEF}"),
                    ("device.default", "true"),
                    ("device.actual-id", "{0.0.1}.{c37f}"),
                    ("wasapi2.device.loopback", "false"),
                ],
            ),
            raw(
                "Default Audio Render Device",
                "Audio/Source",
                &[
                    ("device.api", "wasapi2"),
                    ("device.id", "{E632}"),
                    ("device.default", "true"),
                    ("wasapi2.device.loopback", "true"),
                ],
            ),
            raw(
                "Speakers (2- Realtek(R) Audio)",
                "Audio/Source",
                &[("device.api", "wasapi2"), ("device.id", "{0.0.0}.{0e23}"), ("wasapi2.device.loopback", "true")],
            ),
            raw(
                "Microphone Array (2- Realtek(R) Audio)",
                "Audio/Source",
                &[("device.api", "wasapi2"), ("device.id", "{0.0.1}.{c37f}"), ("wasapi2.device.loopback", "false")],
            ),
        ]
    }

    #[test]
    fn lists_real_inputs_once_with_the_default_microphone_marked() {
        let d = devices(&laptop());
        let summary: Vec<_> = d.iter().map(|d| (d.kind, d.name.as_str(), d.api.as_str(), d.default)).collect();
        assert_eq!(
            summary,
            [
                (Kind::Camera, "Laptop Camera", "mediafoundation", false),
                (Kind::Microphone, "Microphone Array (2- Realtek(R) Audio)", "wasapi2", true),
            ]
        );
    }

    #[test]
    fn two_identical_cameras_stay_two() {
        let cams = [
            raw("USB Camera", "Source/Video", &[("device.api", "mediafoundation"), ("device.path", "path-a")]),
            raw("USB Camera", "Source/Video", &[("device.api", "mediafoundation"), ("device.path", "path-b")]),
        ];
        assert_eq!(devices(&cams).len(), 2);
    }

    #[test]
    fn choice_follows_the_config_and_waits_for_a_missing_device() {
        let d = devices(&laptop());
        assert!(
            matches!(choose(&Selection::Auto, Kind::Camera, &d), Choice::Device { ref name, .. } if name == "Laptop Camera")
        );
        assert!(
            matches!(choose(&Selection::Auto, Kind::Microphone, &d), Choice::Device { ref name, .. } if name.starts_with("Microphone Array"))
        );
        let by_name = Selection::Device { id: "old-id".into(), name: "Laptop Camera".into() };
        assert!(
            matches!(choose(&by_name, Kind::Camera, &d), Choice::Device { .. }),
            "falls back to the name when the id changed"
        );
        let atem = Selection::Device { id: "x".into(), name: "Blackmagic ATEM".into() };
        assert_eq!(choose(&atem, Kind::Camera, &d), Choice::Missing { name: "Blackmagic ATEM".into() });
        assert_eq!(choose(&Selection::Auto, Kind::Camera, &[]), Choice::Missing { name: "any camera".into() });
        assert_eq!(choose(&Selection::None, Kind::Camera, &d), Choice::None);
        assert_eq!(choose(&Selection::Test, Kind::Microphone, &d), Choice::Test);
    }

    #[test]
    fn config_parses_each_selection_and_rejects_bad_files() {
        let dir = std::env::temp_dir().join(format!("jivvy-media-cfg-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load_config(&dir).unwrap().camera, Selection::Auto);
        std::fs::write(
            dir.join(CONFIG_FILE),
            r#"{"version":1,"camera":{"use":"device","id":"p","name":"Cam"},"microphone":{"use":"test"}}"#,
        )
        .unwrap();
        let c = load_config(&dir).unwrap();
        assert_eq!(
            (c.camera, c.microphone),
            (Selection::Device { id: "p".into(), name: "Cam".into() }, Selection::Test)
        );
        std::fs::write(dir.join(CONFIG_FILE), r#"{"version":1,"camera":{"use":"projector"}}"#).unwrap();
        assert!(load_config(&dir).is_err());
    }

    #[test]
    fn meter_values_are_always_finite() {
        assert_eq!(meter_db(f64::NEG_INFINITY), -100.0);
        assert_eq!(meter_db(f64::NAN), -100.0);
        assert_eq!(meter_db(-250.0), -100.0);
        assert_eq!(meter_db(3.0), 0.0);
        assert_eq!(meter_db(-12.5), -12.5);
    }
}
