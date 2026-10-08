//! Camera and microphone choice for `jivvy-video`. Pure logic, tested without devices.
//!
//! Configuration is `media.json` in the data directory; without it the first camera and
//! the computer's default microphone are used, and then kept (`media-memory.json`) until
//! the configuration changes. A chosen device that is unplugged is waited for, never
//! silently swapped for another one (the stream would suddenly show a different camera);
//! the status file reports it so the tech lead can see why.

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
    /// Program feed settings (resolution, frame rate, bitrate, encoder).
    #[serde(default)]
    pub program: crate::program::ProgramConfig,
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

pub const MEMORY_FILE: &str = "media-memory.json";

/// The device `auto` resolved to, kept until `media.json` changes, so unplugging it means
/// waiting for it rather than switching the live picture to another camera.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    pub id: String,
    pub name: String,
}

/// What `jivvy-video` remembers across restarts (`media-memory.json`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    /// Auto pins by kind ("camera", "microphone").
    #[serde(default)]
    pub auto_pins: BTreeMap<String, Pin>,
    /// Device names ever seen on two devices at once, by kind. A configured device that
    /// disappears is never replaced by name for these: the other one is a different device.
    #[serde(default)]
    pub ambiguous_names: BTreeMap<String, std::collections::BTreeSet<String>>,
}

fn kind_key(kind: Kind) -> &'static str {
    if kind == Kind::Camera { "camera" } else { "microphone" }
}

impl Memory {
    /// Reads `media-memory.json`; missing or unreadable starts fresh.
    pub fn load(dir: &Path) -> Memory {
        std::fs::read(dir.join(MEMORY_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Records names currently shared by two or more devices of a kind.
    pub fn observe(&mut self, devices: &[DeviceInfo]) {
        for (i, d) in devices.iter().enumerate() {
            if devices[i + 1..].iter().any(|o| o.kind == d.kind && o.name == d.name && o.id != d.id) {
                self.ambiguous_names.entry(kind_key(d.kind).into()).or_default().insert(d.name.clone());
            }
        }
    }

    /// The device with this id; if the id changed (e.g. a different USB port), the one
    /// device with this name, unless that name has ever belonged to two devices at once.
    fn find<'a>(&self, kind: Kind, id: &str, name: &str, devices: &'a [DeviceInfo]) -> Option<&'a DeviceInfo> {
        let of_kind = || devices.iter().filter(move |d| d.kind == kind);
        if let Some(d) = of_kind().find(|d| d.id == id) {
            return Some(d);
        }
        if self.ambiguous_names.get(kind_key(kind)).is_some_and(|names| names.contains(name)) {
            return None;
        }
        let mut same_name = of_kind().filter(|d| d.name == name);
        match (same_name.next(), same_name.next()) {
            (Some(d), None) => Some(d),
            _ => None,
        }
    }

    /// What to use for one input. `auto` picks the default (or first) device once and then
    /// stays on it; any other selection clears that pin, so the next `auto` picks afresh.
    pub fn choose(&mut self, sel: &Selection, kind: Kind, devices: &[DeviceInfo]) -> Choice {
        let device = |d: &DeviceInfo| Choice::Device { id: d.id.clone(), name: d.name.clone() };
        if *sel != Selection::Auto {
            self.auto_pins.remove(kind_key(kind));
        }
        match sel {
            Selection::None => Choice::None,
            Selection::Test => Choice::Test,
            Selection::Device { id, name } => {
                self.find(kind, id, name, devices).map(device).unwrap_or(Choice::Missing { name: name.clone() })
            }
            Selection::Auto => {
                if let Some(pin) = self.auto_pins.get(kind_key(kind)) {
                    return self
                        .find(kind, &pin.id, &pin.name, devices)
                        .map(device)
                        .unwrap_or(Choice::Missing { name: pin.name.clone() });
                }
                let of_kind = || devices.iter().filter(move |d| d.kind == kind);
                match of_kind().find(|d| d.default).or_else(|| of_kind().next()) {
                    Some(d) => {
                        self.auto_pins.insert(kind_key(kind).into(), Pin { id: d.id.clone(), name: d.name.clone() });
                        device(d)
                    }
                    None => Choice::Missing { name: format!("any {}", kind_key(kind)) },
                }
            }
        }
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
    /// The engine acknowledged the latest level report or check (at least once a second).
    pub connected: bool,
    pub program: crate::program::ProgramStatus,
    pub stream: crate::stream::StreamStatus,
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

    fn cam(name: &str, id: &str) -> DeviceInfo {
        DeviceInfo {
            kind: Kind::Camera,
            id: id.into(),
            name: name.into(),
            api: "mediafoundation".into(),
            default: false,
        }
    }

    #[test]
    fn choice_follows_the_config_and_waits_for_a_missing_device() {
        let d = devices(&laptop());
        let mut m = Memory::default();
        assert!(
            matches!(m.choose(&Selection::Auto, Kind::Camera, &d), Choice::Device { ref name, .. } if name == "Laptop Camera")
        );
        assert!(
            matches!(m.choose(&Selection::Auto, Kind::Microphone, &d), Choice::Device { ref name, .. } if name.starts_with("Microphone Array"))
        );
        let by_name = Selection::Device { id: "old-id".into(), name: "Laptop Camera".into() };
        assert!(
            matches!(m.choose(&by_name, Kind::Camera, &d), Choice::Device { .. }),
            "an unambiguous name finds a device whose id changed (another USB port)"
        );
        let atem = Selection::Device { id: "x".into(), name: "Blackmagic ATEM".into() };
        assert_eq!(m.choose(&atem, Kind::Camera, &d), Choice::Missing { name: "Blackmagic ATEM".into() });
        assert_eq!(
            Memory::default().choose(&Selection::Auto, Kind::Camera, &[]),
            Choice::Missing { name: "any camera".into() }
        );
        assert_eq!(m.choose(&Selection::None, Kind::Camera, &d), Choice::None);
        assert_eq!(m.choose(&Selection::Test, Kind::Microphone, &d), Choice::Test);
    }

    #[test]
    fn auto_stays_on_its_camera_when_it_is_unplugged_until_the_config_changes() {
        let (a, b) = (cam("Front camera", "path-a"), cam("Side camera", "path-b"));
        let mut m = Memory::default();
        let a_choice = Choice::Device { id: "path-a".into(), name: "Front camera".into() };
        assert_eq!(m.choose(&Selection::Auto, Kind::Camera, &[a.clone(), b.clone()]), a_choice);
        // A unplugged, B still there: wait for A, never switch the live picture to B.
        assert_eq!(
            m.choose(&Selection::Auto, Kind::Camera, std::slice::from_ref(&b)),
            Choice::Missing { name: "Front camera".into() }
        );
        // A back: picked up again.
        assert_eq!(m.choose(&Selection::Auto, Kind::Camera, &[b.clone(), a.clone()]), a_choice);
        // The pin survives a restart of the video process.
        let saved: Memory = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        let mut restarted = saved;
        assert_eq!(
            restarted.choose(&Selection::Auto, Kind::Camera, std::slice::from_ref(&b)),
            Choice::Missing { name: "Front camera".into() }
        );
        // Changing the config (here to none, then back to auto) re-resolves.
        restarted.choose(&Selection::None, Kind::Camera, std::slice::from_ref(&b));
        assert!(
            matches!(restarted.choose(&Selection::Auto, Kind::Camera, std::slice::from_ref(&b)), Choice::Device { ref id, .. } if id == "path-b")
        );
    }

    #[test]
    fn unplugging_one_of_two_identical_cameras_never_switches_to_the_other() {
        let (a, b) = (cam("USB Camera", "path-a"), cam("USB Camera", "path-b"));
        let configured = Selection::Device { id: "path-a".into(), name: "USB Camera".into() };
        let mut m = Memory::default();
        m.observe(&[a.clone(), b.clone()]);
        assert!(
            matches!(m.choose(&configured, Kind::Camera, &[a, b.clone()]), Choice::Device { ref id, .. } if id == "path-a")
        );
        assert_eq!(
            m.choose(&configured, Kind::Camera, std::slice::from_ref(&b)),
            Choice::Missing { name: "USB Camera".into() },
            "the name has belonged to two devices, so the other one is not a substitute"
        );
        // The same holds for an auto pin.
        let mut m2 = m.clone();
        m2.auto_pins.insert("camera".into(), Pin { id: "path-a".into(), name: "USB Camera".into() });
        assert_eq!(m2.choose(&Selection::Auto, Kind::Camera, &[b]), Choice::Missing { name: "USB Camera".into() });
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
