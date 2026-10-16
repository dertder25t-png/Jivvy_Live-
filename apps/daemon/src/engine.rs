//! The live engine: owns the service state and answers protocol commands.
//!
//! Every change is saved to the snapshot store before it is acknowledged, so whatever
//! an operator saw acknowledged survives a crash. Connections that send `state.subscribe`
//! (output windows, operator screens) get a `state` event after every change. Video and
//! streaming plug in here in later items; until then `stream.start` answers `unavailable`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::devices::{self, Devices};
use crate::protocol::{
    self, Ack, AckExtras, Command, DeviceInfo, ErrorCode, PairRequestInfo, PairingStatus, RecordingStatus, RequestAck,
    StateSnapshot, StreamDestination, StreamStatus,
};
use crate::snapshot::{Snapshot, Store};
use crate::tls::TlsFiles;

/// Longest message on either command channel (a TCP line or a WebSocket message): room for
/// a whole run sheet (`runsheet.set`). The same as the TypeScript `MAX_MESSAGE_BYTES`, which
/// UIs check before sending.
const MAX_MESSAGE: usize = 1024 * 1024;
/// Messages queued for one connection before it counts as stuck and is disconnected.
const OUTBOX_CAPACITY: usize = 256;
/// A stream report older than this means the video process isn't reporting (crashed or
/// restarting): screens see "reconnecting", never a stale "live".
pub const STREAM_REPORT_STALE: Duration = Duration::from_secs(3);
/// The same for the recording: a silent video process is never shown as recording.
pub const RECORDING_REPORT_STALE: Duration = Duration::from_secs(3);

/// Where messages for one connection go. All writes to a socket go through its outbox so
/// acks and events never interleave mid-line.
#[derive(Clone)]
pub struct Outbox {
    /// Tells connections apart (one may both subscribe and ask for a result).
    id: u64,
    tx: SyncSender<Vec<u8>>,
    /// Closed if the client stops reading, so it reconnects and gets fresh state.
    stream: Option<Arc<TcpStream>>,
}

impl Outbox {
    pub fn new(tx: SyncSender<Vec<u8>>, stream: Option<Arc<TcpStream>>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Outbox { id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), tx, stream }
    }
}

/// Who is on the other end of a connection.
///
/// The local channel (TCP on loopback) is the church computer itself: our own processes and
/// its screen, all trusted. Every WebSocket connection is a device on the network, which
/// must be paired and prove it holds its key before anything else.
pub struct Session {
    id: u64,
    local: bool,
    /// Set once the device has proven its key.
    device_id: Option<String>,
    /// The key and challenge from the last `auth.hello`; one proof per challenge.
    hello: Option<(String, String)>,
}

impl Session {
    pub fn local() -> Session {
        Session::new(true)
    }

    pub fn device() -> Session {
        Session::new(false)
    }

    fn new(local: bool) -> Session {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Session { id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), local, device_id: None, hello: None }
    }
}

/// What a paired device may do: run the service. Pairing, devices and the video process's
/// reports are the church computer's alone.
fn device_may(command: &Command) -> bool {
    matches!(
        command,
        Command::SlideNext
            | Command::SlidePrev
            | Command::SlideGoto { .. }
            | Command::OutputBlack { .. }
            | Command::StreamStart
            | Command::StreamStop
            | Command::RecordingStart
            | Command::RecordingStop
            | Command::StateGet
            | Command::StateSubscribe
            | Command::StreamPreflight
            | Command::RunSheetSet { .. }
            | Command::RunSheetGet
            | Command::MediaPut { .. }
            | Command::MediaHas { .. }
            | Command::DocList { .. }
            | Command::DocGet { .. }
            | Command::DocPut { .. }
            | Command::DocDelete { .. }
    )
}

struct PendingPairing {
    request_id: String,
    name: String,
    public_key: String,
    code: String,
    session: u64,
    outbox: Option<Outbox>,
}

pub struct Engine {
    state: Snapshot,
    store: Store,
    /// Slides to count to when no run sheet is set (`--slides`).
    slide_count: u64,
    /// The run sheet and its revision, saved in `runsheet.json`.
    run_sheet: Option<crate::runsheet::Saved>,
    /// Images arriving in pieces.
    uploads: crate::images::Uploads,
    /// The latest clean-up of unused images, running on its own thread.
    pruning: Option<std::thread::JoinHandle<()>>,
    /// Images no clean-up may delete: what the run sheet shows, plus every image a device has
    /// sent, asked about or named since. Each clean-up checks it just before each delete, so
    /// an older one still running can never remove an image a newer run sheet shows.
    protected: Arc<Mutex<std::collections::HashSet<String>>>,
    /// The slide on screen and since when, for items that move on by themselves.
    shown_since: Option<(u64, Instant)>,
    /// Songs, plans, themes and settings (`library.sqlite3`); None if it couldn't be opened,
    /// which never stops a service: the doc commands answer `unavailable`.
    library: Option<crate::library::Library>,
    subscribers: Vec<Outbox>,
    /// The church computer's subscribers, which also hear about pairing.
    local_subscribers: Vec<Outbox>,
    devices: Devices,
    daemon_id: String,
    /// Open for pairing requests until then.
    pairing_until: Option<Instant>,
    pending: Vec<PendingPairing>,
    /// Connections whose device has proven its key: (device, session), and how to cut them off.
    proven: Vec<(String, u64, Option<Outbox>)>,
    /// The video process's latest report on the stream, and when it came.
    stream_report: Option<(StreamStatus, Instant)>,
    /// Each platform, from the latest report that listed them.
    stream_destinations: Vec<StreamDestination>,
    /// The video process's latest report on the recording: its status, whether the disk is
    /// running low, and when it came.
    recording_report: Option<(RecordingStatus, bool, Instant)>,
    /// This engine, for work that finishes later on its own thread (the upload test).
    handle: Option<Weak<Mutex<Engine>>>,
    upload_test_url: String,
    /// Set while the upload test runs; storing true stops it.
    preflight_cancel: Option<Arc<AtomicBool>>,
    /// Connections that asked for the result, which they get even if not subscribed.
    preflight_askers: Vec<Outbox>,
    /// The view subscribers last received, so `tick` only sends real changes.
    last_sent: Option<StateSnapshot>,
}

impl Engine {
    pub fn new(store: Store, state: Snapshot, slide_count: u64) -> Self {
        let run_sheet = crate::runsheet::load(store.dir());
        let mut state = state;
        // The slide number belongs to another run sheet: the new one starts at its beginning.
        if let Some(saved) = &run_sheet
            && state.run_sheet_id.as_deref() != Some(saved.run_sheet.id.as_str())
        {
            state.slide_index = 0;
            state.run_sheet_id = Some(saved.run_sheet.id.clone());
            // A stop pressed in the old run sheet doesn't hold a loop in the new one.
            state.loop_stopped = false;
        }
        // A snapshot from a longer run sheet must not point past the end of this one.
        let count = run_sheet.as_ref().map_or(slide_count, |s| s.run_sheet.slides().count() as u64).max(1);
        state.slide_index = state.slide_index.min(count - 1);
        let devices = Devices::load(store.dir());
        let daemon_id = devices::daemon_id(store.dir()).unwrap_or_else(|e| {
            // Devices check this id; one that changes makes them re-pair, never lets anyone in.
            crate::log("engine", format!("can't save the daemon id ({e}); using a temporary one"));
            devices::random_id(16)
        });
        let library = crate::library::Library::open(store.dir())
            .map_err(|e| crate::log("engine", format!("the library can't be opened: {e:?}")))
            .ok();
        let mut engine = Engine {
            state,
            store,
            slide_count: slide_count.max(1),
            run_sheet,
            uploads: Default::default(),
            pruning: None,
            protected: Default::default(),
            shown_since: None,
            library,
            subscribers: Vec::new(),
            local_subscribers: Vec::new(),
            devices,
            daemon_id,
            pairing_until: None,
            pending: Vec::new(),
            proven: Vec::new(),
            handle: None,
            upload_test_url: crate::preflight::DEFAULT_URL.into(),
            preflight_cancel: None,
            preflight_askers: Vec::new(),
            stream_report: None,
            stream_destinations: Vec::new(),
            recording_report: None,
            last_sent: None,
        };
        // Left from before a crash: images no run sheet shows, pieces of uploads that stopped.
        engine.prune_images();
        engine
    }

    pub fn state(&self) -> &Snapshot {
        &self.state
    }

    /// How many slides there are: the run sheet's, else `--slides`. Never 0.
    fn slide_count(&self) -> u64 {
        self.run_sheet.as_ref().map_or(self.slide_count, |s| s.run_sheet.slides().count() as u64).max(1)
    }

    pub fn view(&self) -> StateSnapshot {
        self.view_at(Instant::now())
    }

    /// The state screens see. The stream is "off" unless asked for; when asked for, it is
    /// "live" only while the video process keeps reporting that it is.
    pub fn view_at(&self, now: Instant) -> StateSnapshot {
        let report_fresh =
            self.stream_report.is_some_and(|(_, at)| now.saturating_duration_since(at) < STREAM_REPORT_STALE);
        // Platforms are only shown while streaming is wanted and the video process is reporting.
        let stream_destinations =
            if self.state.stream_wanted && report_fresh { self.stream_destinations.clone() } else { Vec::new() };
        let stream = match (self.state.stream_wanted, self.stream_report) {
            (false, _) => StreamStatus::Off,
            (true, Some((StreamStatus::Live, at))) if now.saturating_duration_since(at) < STREAM_REPORT_STALE => {
                StreamStatus::Live
            }
            (true, _) => StreamStatus::Reconnecting,
        };
        // Recording only while the video process keeps saying it is writing; the disk
        // warning needs the same fresh report, and only matters while recording is wanted.
        let fresh =
            self.recording_report.filter(|(_, _, at)| now.saturating_duration_since(*at) < RECORDING_REPORT_STALE);
        let (recording, recording_low_disk) = match (self.state.recording_wanted, fresh) {
            (false, _) => (RecordingStatus::Off, false),
            (true, Some((RecordingStatus::Recording, low, _))) => (RecordingStatus::Recording, low),
            (true, Some((_, low, _))) => (RecordingStatus::Waiting, low),
            (true, None) => (RecordingStatus::Waiting, false),
        };
        StateSnapshot {
            slide_index: self.state.slide_index,
            black: self.state.black,
            stream,
            stream_wanted: self.state.stream_wanted,
            recording,
            recording_wanted: self.state.recording_wanted,
            recording_low_disk,
            stream_destinations,
            run_sheet_id: self.run_sheet.as_ref().map(|s| s.run_sheet.id.clone()),
            run_sheet_rev: self.run_sheet.as_ref().map(|s| s.rev),
            slide_count: self.run_sheet.as_ref().map(|_| self.slide_count()),
            // An empty run sheet shows a blank slide, not "Slide 1".
            slide: self
                .run_sheet
                .as_ref()
                .map(|s| s.run_sheet.slides().nth(self.state.slide_index as usize).cloned().unwrap_or_default()),
        }
    }

    /// Called a few times a second: tells subscribers when the view changed by itself
    /// (a stream or recording report going stale).
    pub fn tick(&mut self) {
        self.tick_at(Instant::now());
    }

    pub fn tick_at(&mut self, now: Instant) {
        self.advance_loop(now);
        if self.last_sent.as_ref() != Some(&self.view_at(now)) {
            self.broadcast_at(now);
        }
        if self.pairing_until.is_some_and(|t| now >= t) {
            self.pairing_until = None;
            self.pairing_changed(now);
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    /// Handles one raw message and returns the ack to send back.
    pub fn handle_line(&mut self, line: &str) -> Ack {
        self.handle_line_from(line, None)
    }

    /// Like `handle_line`, for a connection on the local channel that can receive events
    /// through `outbox`.
    pub fn handle_line_from(&mut self, line: &str, outbox: Option<&Outbox>) -> Ack {
        self.handle(line, &mut Session::local(), outbox)
    }

    /// Handles one raw message from `session`.
    pub fn handle(&mut self, line: &str, session: &mut Session, outbox: Option<&Outbox>) -> Ack {
        let env = match protocol::parse_envelope(line) {
            Ok(e) => e,
            Err(e) => return protocol::nack(e.id.as_deref().unwrap_or(""), e.code, e.message),
        };
        let id = env.id.as_str();
        if !session.local {
            match env.command {
                Command::PairRequest { public_key, name } => {
                    return self.pair_request(id, session, outbox, public_key, name);
                }
                Command::AuthHello { public_key } => {
                    let challenge = devices::random_id(32);
                    session.hello = Some((public_key, challenge.clone()));
                    let extras = AckExtras {
                        challenge: Some(challenge),
                        daemon_id: Some(self.daemon_id.clone()),
                        ..Default::default()
                    };
                    return protocol::ack_with(id, None, extras);
                }
                Command::AuthProve { signature } => return self.auth_prove(id, session, outbox, &signature),
                _ => {}
            }
            // Checked on every command, so a device removed a moment ago gets nothing more.
            let Some(device) = session.device_id.as_deref().filter(|d| self.devices.contains_id(d)) else {
                return protocol::nack(id, ErrorCode::Unauthorized, "pair this device first");
            };
            if !device_may(&env.command) {
                crate::log("engine", format!("refused {} from device {device}", command_name(&env.command)));
                return protocol::nack(id, ErrorCode::Unauthorized, "only the church computer can do that");
            }
        }
        let mut next = self.state.clone();
        let mut stop_loop = false;
        match env.command {
            Command::PairOpen => {
                self.pairing_until = Some(Instant::now() + protocol::PAIRING_WINDOW);
                crate::log("engine", "pairing open for 2 minutes");
                self.pairing_changed(Instant::now());
                return self.pairing_ack(id);
            }
            Command::PairClose => {
                self.pairing_until = None;
                self.pairing_changed(Instant::now());
                return self.pairing_ack(id);
            }
            Command::PairApprove { request_id } => return self.pair_answer(id, &request_id, true),
            Command::PairDeny { request_id } => return self.pair_answer(id, &request_id, false),
            Command::DeviceList => return self.devices_ack(id),
            Command::DeviceRemove { device_id } => {
                match self.devices.remove(&device_id) {
                    Ok(true) => {}
                    Ok(false) => return protocol::nack(id, ErrorCode::BadArguments, "no such device"),
                    Err(e) => {
                        crate::log("engine", format!("devices.json save failed: {e}"));
                        return protocol::nack(id, ErrorCode::Unavailable, "could not save; try again");
                    }
                }
                crate::log("engine", format!("device {device_id} removed"));
                // Its open connections end now, not at their next command.
                self.proven.retain(|(device, _, outbox)| {
                    let removed = *device == device_id;
                    if let Some(stream) = outbox.as_ref().and_then(|o| o.stream.as_ref()).filter(|_| removed) {
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    !removed
                });
                return self.devices_ack(id);
            }
            Command::StreamPreflight => {
                // The test would take the stream's bandwidth.
                if self.state.stream_wanted {
                    return protocol::nack(id, ErrorCode::Unavailable, "the upload test can't run while streaming");
                }
                return self.start_preflight(id, outbox);
            }
            Command::PairRequest { .. } | Command::AuthHello { .. } | Command::AuthProve { .. } => {
                return protocol::nack(id, ErrorCode::BadArguments, "the church computer doesn't need to pair");
            }
            Command::RunSheetGet => {
                let extras =
                    AckExtras { run_sheet: self.run_sheet.as_ref().map(|s| s.run_sheet.clone()), ..Default::default() };
                return protocol::ack_with(id, Some(self.view()), extras);
            }
            Command::RunSheetSet { run_sheet } => return self.set_run_sheet(id, run_sheet),
            Command::MediaPut { id: media, offset, data, last } => {
                return self.media_put(id, &media, offset, &data, last);
            }
            doc @ (Command::DocList { .. }
            | Command::DocGet { .. }
            | Command::DocPut { .. }
            | Command::DocDelete { .. }) => {
                return self.doc(id, doc);
            }
            Command::MediaHas { ids } => {
                // A device skips sending what is here, so it has to stay until the run sheet.
                self.protect(ids.iter().cloned());
                let missing = ids.into_iter().filter(|m| !crate::images::has(self.store.dir(), m)).collect();
                return protocol::ack_with(id, None, AckExtras { missing: Some(missing), ..Default::default() });
            }
            Command::SlideNext => {
                let target = self.next_slide();
                next.slide_index = target.min(self.slide_count() - 1);
                // A loop at the end of the run sheet: nothing after it, so Next goes to its
                // last slide and stops the loop there (saved, so a restart doesn't restart it).
                stop_loop = target >= self.slide_count() && self.in_loop(self.state.slide_index);
            }
            Command::SlidePrev => next.slide_index = next.slide_index.saturating_sub(1),
            Command::SlideGoto { index } => {
                if index >= self.slide_count() {
                    return protocol::nack(id, ErrorCode::BadArguments, "slide out of range");
                }
                next.slide_index = index;
            }
            Command::OutputBlack { on } => next.black = on,
            // Saved like the slide, so a crash mid-service resumes streaming by itself.
            Command::StreamStart => {
                // A running upload test stops: the stream needs the bandwidth.
                if let Some(cancel) = &self.preflight_cancel {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                next.stream_wanted = true
            }
            Command::StreamStop => next.stream_wanted = false,
            Command::StreamReport { status, destinations } => {
                let now = Instant::now();
                if let Some(destinations) = destinations {
                    // Compared only with a report that is still current: after a gap (the video
                    // process restarting) everything starts afresh, which isn't news.
                    let fresh = self
                        .stream_report
                        .is_some_and(|(_, at)| now.saturating_duration_since(at) < STREAM_REPORT_STALE);
                    if fresh && self.state.stream_wanted {
                        for alert in protocol::stream_alerts(&self.stream_destinations, &destinations) {
                            crate::log("engine", format!("alert: {}", alert.message));
                            if let Ok(line) = serde_json::to_vec(&protocol::alert_event(&alert)) {
                                self.send_to_subscribers(line);
                            }
                        }
                    }
                    self.stream_destinations = destinations;
                } else {
                    // A video process that doesn't list them (an older one): nothing to show,
                    // rather than another process's last list.
                    self.stream_destinations.clear();
                }
                // Live status, not saved: it only means anything while it keeps coming.
                self.stream_report = Some((status, now));
                self.tick();
                return protocol::ack(id, None);
            }
            // Saved like the stream: a crash mid-service resumes the recording by itself.
            Command::RecordingStart => next.recording_wanted = true,
            Command::RecordingStop => next.recording_wanted = false,
            Command::RecordingReport { status, low_disk } => {
                self.recording_report = Some((status, low_disk, Instant::now()));
                self.tick();
                return protocol::ack(id, None);
            }
            Command::StateGet => {}
            Command::MediaLevels(levels) => {
                // Live meter data: relayed, never saved, never a state change.
                if let Ok(line) = serde_json::to_vec(&protocol::levels_event(&levels)) {
                    self.send_to_subscribers(line);
                }
                return protocol::ack(id, None);
            }
            Command::StateSubscribe => match outbox {
                Some(o) => {
                    self.subscribers.push(o.clone());
                    if session.local {
                        self.local_subscribers.push(o.clone());
                        // Started (or reconnected) while pairing is open: it still sees the
                        // requests already waiting.
                        if self.pairing_open(Instant::now()) {
                            send_event(Some(o), &protocol::pairing_event(self.pairing_status(Instant::now())));
                        }
                    }
                }
                None => return protocol::nack(id, ErrorCode::Unavailable, "this connection can't receive events"),
            },
        }
        // Any other slide change starts a stopped loop's slide moving again.
        if stop_loop {
            next.loop_stopped = true;
        } else if next.slide_index != self.state.slide_index {
            next.loop_stopped = false;
        }
        if next != self.state {
            next.saved_at = crate::now_ms();
            if let Err(e) = self.store.save(&next) {
                // Don't acknowledge a change we couldn't make crash-safe.
                crate::log("engine", format!("snapshot save failed: {e}"));
                return protocol::nack(id, ErrorCode::Unavailable, "could not save state; try again");
            }
            if next.slide_index != self.state.slide_index {
                // Moved by the operator: a loop's wait starts again from the next tick, even if
                // they come straight back to the same slide before it.
                self.shown_since = None;
            }
            self.state = next;
            self.broadcast();
        }
        protocol::ack(id, Some(self.view()))
    }

    /// Whether slide `index` belongs to a pre-service loop.
    fn in_loop(&self, index: u64) -> bool {
        self.run_sheet
            .as_ref()
            .and_then(|s| s.run_sheet.item_at(index))
            .is_some_and(|(i, _)| i.advance_seconds.is_some())
    }

    /// Where Next goes: the following slide, except from a pre-service loop, which Next leaves
    /// for the item after it (pressing Next to start the service must never keep it looping).
    fn next_slide(&self) -> u64 {
        let index = self.state.slide_index;
        match self.run_sheet.as_ref().and_then(|s| s.run_sheet.item_at(index)) {
            Some((item, start)) if item.advance_seconds.is_some() => start + item.slides.len() as u64,
            _ => index + 1,
        }
    }

    /// The pre-service loop: on a slide of an item with `advanceSeconds`, moves to that item's
    /// next slide once it has shown that long, and from its last back to its first. Any other
    /// slide change (Next, Back, a new run sheet) restarts the wait.
    fn advance_loop(&mut self, now: Instant) {
        let index = self.state.slide_index;
        let since = match self.shown_since {
            Some((shown, since)) if shown == index => since,
            _ => {
                self.shown_since = Some((index, now));
                return;
            }
        };
        if self.state.loop_stopped {
            return;
        }
        let Some((item, start)) = self.run_sheet.as_ref().and_then(|s| s.run_sheet.item_at(index)) else { return };
        let Some(secs) = item.advance_seconds.filter(|_| item.slides.len() > 1) else { return };
        if now.saturating_duration_since(since) < Duration::from_secs(secs) {
            return;
        }
        let mut next = self.state.clone();
        next.slide_index = start + (index - start + 1) % item.slides.len() as u64;
        next.saved_at = crate::now_ms();
        // Saved like any slide change, so a restart carries on in the loop where it was. Not
        // shown unless saved: the screens stay put and the next tick tries again.
        if let Err(e) = self.store.save(&next) {
            crate::log("engine", format!("snapshot save failed: {e}"));
            return;
        }
        self.state = next;
        self.shown_since = Some((self.state.slide_index, now));
        self.broadcast_at(now);
    }

    /// Saves the new run sheet, then moves to its first slide (another run sheet) or stays on
    /// the current one (an edit of this one, clamped to its end), and tells every screen.
    /// One piece of an image from a device (see `images::put`).
    fn media_put(&mut self, id: &str, media: &str, offset: u64, data: &str, last: bool) -> Ack {
        use base64::Engine as _;
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
            return protocol::nack(id, ErrorCode::BadArguments, "media.put data isn't base64");
        };
        if bytes.len() > protocol::MEDIA_CHUNK_BYTES {
            return protocol::nack(id, ErrorCode::BadArguments, "media.put piece too big");
        }
        self.protect([media.to_string()]);
        match crate::images::put(&mut self.uploads, self.store.dir(), media, offset, &bytes, last) {
            Ok(progress) => {
                if progress.complete && last {
                    crate::log("engine", format!("image {} received ({} bytes)", &media[..12], progress.received));
                }
                protocol::ack_with(id, None, AckExtras { media: Some(progress), ..Default::default() })
            }
            Err(crate::images::PutError::Offset(have)) => {
                protocol::nack(id, ErrorCode::BadArguments, format!("send this image from offset {have}"))
            }
            Err(crate::images::PutError::TooBig) => protocol::nack(
                id,
                ErrorCode::BadArguments,
                format!("images can be at most {} MB", protocol::MAX_MEDIA_BYTES / (1024 * 1024)),
            ),
            Err(crate::images::PutError::Corrupt(why)) => protocol::nack(id, ErrorCode::BadArguments, why),
            Err(crate::images::PutError::Full) => protocol::nack(
                id,
                ErrorCode::Unavailable,
                "the church computer's picture storage is full (2 GB); send a run sheet without old pictures first",
            ),
            Err(crate::images::PutError::Io(e)) => {
                crate::log("engine", format!("image save failed: {e}"));
                protocol::nack(id, ErrorCode::Unavailable, "could not save the image; try again")
            }
        }
    }

    fn set_run_sheet(&mut self, id: &str, run_sheet: protocol::RunSheet) -> Ack {
        // Protected before the check, so no clean-up still running removes one after it.
        self.protect(run_sheet.slides().filter_map(|s| s.image.clone()));
        // Every image must already be here: a slide never shows something the screens can't load.
        if let Some(image) =
            run_sheet.slides().filter_map(|s| s.image.as_deref()).find(|i| !crate::images::has(self.store.dir(), i))
        {
            let message =
                format!("the church computer doesn't have image {} yet; send it with media.put first", &image[..12]);
            return protocol::nack(id, ErrorCode::BadArguments, message);
        }
        let same = self.run_sheet.as_ref().is_some_and(|s| s.run_sheet.id == run_sheet.id);
        let rev = self.run_sheet.as_ref().map_or(1, |s| s.rev + 1);
        let saved = crate::runsheet::Saved { version: 1, rev, run_sheet };
        if let Err(e) = crate::runsheet::save(self.store.dir(), &saved) {
            crate::log("engine", format!("runsheet.json save failed: {e}"));
            return protocol::nack(id, ErrorCode::Unavailable, "could not save the run sheet; try again");
        }
        crate::log("engine", format!("run sheet {} rev {rev}", saved.run_sheet.id));
        let mut next = self.state.clone();
        next.run_sheet_id = Some(saved.run_sheet.id.clone());
        next.loop_stopped = false;
        self.run_sheet = Some(saved);
        // A loop in the new run sheet gets its full time on its first slide, even if the
        // slide number is the same as before.
        self.shown_since = None;
        next.slide_index = if same { next.slide_index.min(self.slide_count() - 1) } else { 0 };
        if next != self.state {
            next.saved_at = crate::now_ms();
            // Safe even if this save fails or the engine dies first: the snapshot still names the
            // old run sheet, so a restart starts the new one on its first slide (an edit of the
            // same one is clamped the same way), which is what screens are shown now.
            if let Err(e) = self.store.save(&next) {
                crate::log("engine", format!("snapshot save failed: {e}"));
            }
            self.state = next;
        }
        self.broadcast();
        self.prune_images();
        protocol::ack(id, Some(self.view()))
    }

    /// Keeps only the images the run sheet shows (and uploads arriving now), so the folder
    /// never grows week after week.
    /// Runs on its own thread: deleting many files must never hold the engine's lock (the
    /// heartbeat waits on it).
    fn prune_images(&mut self) {
        let keep: std::collections::HashSet<String> =
            self.run_sheet.iter().flat_map(|s| s.run_sheet.slides()).filter_map(|s| s.image.clone()).collect();
        let (dir, arriving) = (self.store.dir().to_path_buf(), self.uploads.ids());
        // From now on only these are protected, plus whatever devices send or name next.
        *self.protected.lock().unwrap_or_else(|p| p.into_inner()) = keep.union(&arriving).cloned().collect();
        let protected = self.protected.clone();
        self.pruning = Some(std::thread::spawn(move || {
            let removed = crate::images::prune(&dir, &protected, &arriving);
            if removed > 0 {
                crate::log("engine", format!("removed {removed} images the run sheet no longer shows"));
            }
        }));
    }

    fn protect(&self, ids: impl IntoIterator<Item = String>) {
        self.protected.lock().unwrap_or_else(|p| p.into_inner()).extend(ids);
    }

    /// Tests: waits for the latest clean-up to finish.
    #[cfg(test)]
    fn wait_for_pruning(&mut self) {
        if let Some(t) = self.pruning.take() {
            t.join().unwrap();
        }
    }

    /// A connection ended: its pairing request and proof go with it.
    pub fn forget(&mut self, session: &Session) {
        self.proven.retain(|(_, s, _)| *s != session.id);
        let before = self.pending.len();
        self.pending.retain(|p| p.session != session.id);
        if self.pending.len() != before {
            self.pairing_changed(Instant::now());
        }
    }

    /// Lets the engine finish work on other threads (the upload test) and tell everyone.
    pub fn set_handle(&mut self, handle: Weak<Mutex<Engine>>) {
        self.handle = Some(handle);
    }

    pub fn set_upload_test_url(&mut self, url: impl Into<String>) {
        self.upload_test_url = url.into();
    }

    /// Starts the upload test on its own thread; the result goes to every subscriber.
    fn start_preflight(&mut self, id: &str, outbox: Option<&Outbox>) -> Ack {
        let measuring = AckExtras { preflight: Some(crate::preflight::measuring()), ..Default::default() };
        if self.handle.is_some() {
            self.preflight_askers.extend(outbox.cloned());
        }
        if self.preflight_cancel.is_some() {
            return protocol::ack_with(id, Some(self.view()), measuring);
        }
        let Some(handle) = self.handle.clone() else {
            return protocol::nack(id, ErrorCode::Unavailable, "the upload test can't run here");
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.preflight_cancel = Some(cancel.clone());
        let (dir, url) = (self.store.dir().to_path_buf(), self.upload_test_url.clone());
        crate::log("engine", "pre-flight: testing the upload");
        std::thread::spawn(move || {
            let result = crate::preflight::run(&dir, &url, cancel);
            let Some(engine) = handle.upgrade() else { return };
            let mut e = engine.lock().unwrap_or_else(|p| p.into_inner());
            e.preflight_cancel = None;
            crate::log("engine", format!("pre-flight: {} {}", result.summary, result.message.as_deref().unwrap_or("")));
            if let Ok(line) = serde_json::to_vec(&protocol::preflight_event(&result)) {
                // Those who asked and aren't subscribed get it directly.
                let mut direct = line.clone();
                direct.push(b'\n');
                for asker in std::mem::take(&mut e.preflight_askers) {
                    if !e.subscribers.iter().any(|s| s.id == asker.id) {
                        let _ = asker.tx.try_send(direct.clone());
                    }
                }
                e.send_to_subscribers(line);
            }
        });
        protocol::ack_with(id, Some(self.view()), measuring)
    }

    pub fn daemon_id(&self) -> &str {
        &self.daemon_id
    }

    fn pairing_open(&self, now: Instant) -> bool {
        self.pairing_until.is_some_and(|t| now < t)
    }

    fn pairing_status(&self, now: Instant) -> PairingStatus {
        match self.pairing_until.filter(|t| now < *t) {
            None => PairingStatus { open: false, closes_in_ms: None, requests: Vec::new() },
            Some(until) => PairingStatus {
                open: true,
                closes_in_ms: Some(until.saturating_duration_since(now).as_millis() as u64),
                requests: self
                    .pending
                    .iter()
                    .map(|p| PairRequestInfo {
                        request_id: p.request_id.clone(),
                        name: p.name.clone(),
                        code: p.code.clone(),
                    })
                    .collect(),
            },
        }
    }

    fn pairing_ack(&self, id: &str) -> Ack {
        let extras = AckExtras { pairing: Some(self.pairing_status(Instant::now())), ..Default::default() };
        protocol::ack_with(id, None, extras)
    }

    fn devices_ack(&self, id: &str) -> Ack {
        let devices = self
            .devices
            .list()
            .iter()
            .map(|d| DeviceInfo { device_id: d.device_id.clone(), name: d.name.clone(), paired_at: d.paired_at })
            .collect();
        protocol::ack_with(id, None, AckExtras { devices: Some(devices), ..Default::default() })
    }

    /// Tells the church computer's subscribers. Once pairing has closed, waiting devices hear
    /// they weren't approved.
    fn pairing_changed(&mut self, now: Instant) {
        if !self.pairing_open(now) {
            for p in std::mem::take(&mut self.pending) {
                send_event(p.outbox.as_ref(), &protocol::pair_result_event(None));
            }
        }
        if let Ok(mut line) = serde_json::to_vec(&protocol::pairing_event(self.pairing_status(now))) {
            line.push(b'\n');
            self.local_subscribers.retain(|s| s.tx.try_send(line.clone()).is_ok());
        }
    }

    fn pair_request(&mut self, id: &str, session: &Session, outbox: Option<&Outbox>, key: String, name: String) -> Ack {
        let now = Instant::now();
        if !self.pairing_open(now) {
            return protocol::nack(id, ErrorCode::Unauthorized, "pairing isn't open; open it on the church computer");
        }
        // Asking again from the same connection replaces the earlier request.
        self.pending.retain(|p| p.session != session.id);
        if self.pending.len() >= protocol::MAX_PENDING_PAIRINGS {
            return protocol::nack(id, ErrorCode::Unauthorized, "too many pairing requests waiting");
        }
        let (request_id, code) = (devices::random_id(12), devices::random_code());
        crate::log("engine", format!("pairing request {request_id} from {name:?}"));
        self.pending.push(PendingPairing {
            request_id: request_id.clone(),
            name,
            public_key: key,
            code: code.clone(),
            session: session.id,
            outbox: outbox.cloned(),
        });
        self.pairing_changed(now);
        protocol::ack_with(id, None, AckExtras { request: Some(RequestAck { request_id, code }), ..Default::default() })
    }

    fn pair_answer(&mut self, id: &str, request_id: &str, approve: bool) -> Ack {
        let now = Instant::now();
        let found = self.pending.iter().position(|p| p.request_id == request_id).filter(|_| self.pairing_open(now));
        let Some(i) = found else {
            return protocol::nack(id, ErrorCode::BadArguments, "no such pairing request");
        };
        if approve {
            let p = &self.pending[i];
            match self.devices.add(&p.name, &p.public_key, crate::now_ms()) {
                Ok(device_id) => {
                    crate::log("engine", format!("paired {:?} as device {device_id}", p.name));
                    send_event(p.outbox.as_ref(), &protocol::pair_result_event(Some((&device_id, &self.daemon_id))));
                }
                Err(e) => {
                    // Not saved means not paired; the request stays so it can be approved again.
                    crate::log("engine", format!("devices.json save failed: {e}"));
                    return protocol::nack(id, ErrorCode::Unavailable, "could not save; try again");
                }
            }
        } else {
            send_event(self.pending[i].outbox.as_ref(), &protocol::pair_result_event(None));
        }
        self.pending.remove(i);
        self.pairing_changed(now);
        self.pairing_ack(id)
    }

    fn auth_prove(&mut self, id: &str, session: &mut Session, outbox: Option<&Outbox>, signature: &str) -> Ack {
        // One proof per challenge, right or wrong.
        let Some((key, challenge)) = session.hello.take() else {
            return protocol::nack(id, ErrorCode::Unauthorized, "send auth.hello first");
        };
        let message = protocol::auth_message(&self.daemon_id, &challenge);
        let device = self.devices.by_key(&key).filter(|_| devices::verify(&key, &message, signature));
        let Some(device_id) = device.map(|d| d.device_id.clone()) else {
            return protocol::nack(id, ErrorCode::Unauthorized, "not a paired device");
        };
        session.device_id = Some(device_id.clone());
        self.proven.push((device_id.clone(), session.id, outbox.cloned()));
        protocol::ack_with(id, Some(self.view()), AckExtras { device_id: Some(device_id), ..Default::default() })
    }

    /// The library's commands. Saves are on disk before the ack; every device subscribed hears
    /// `doc.changed`, so its lists follow edits made elsewhere.
    fn doc(&mut self, id: &str, command: Command) -> Ack {
        use crate::library::Error;
        let Some(lib) = self.library.as_mut() else {
            return protocol::nack(
                id,
                ErrorCode::Unavailable,
                "the church computer's library can't be opened; see its log",
            );
        };
        let failed = |e: Error| match e {
            Error::Conflict { current } => protocol::nack(
                id,
                ErrorCode::Conflict,
                format!("changed on another device (now revision {current}); load it again before saving"),
            ),
            Error::Db(e) => {
                crate::log("engine", format!("library: {e}"));
                protocol::nack(id, ErrorCode::Unavailable, "the library couldn't save that; try again")
            }
        };
        let changed = match command {
            Command::DocList { kind } => {
                return match lib.list(&kind) {
                    Ok(docs) => protocol::ack_with(id, None, AckExtras { docs: Some(docs), ..Default::default() }),
                    Err(e) => failed(e),
                };
            }
            Command::DocGet { kind, id: doc_id } => {
                return match lib.get(&kind, &doc_id) {
                    Ok(Some(doc)) => protocol::ack_with(id, None, AckExtras { doc: Some(doc), ..Default::default() }),
                    Ok(None) => protocol::nack(id, ErrorCode::BadArguments, format!("there is no {kind} {doc_id}")),
                    Err(e) => failed(e),
                };
            }
            Command::DocPut { kind, id: doc_id, title, body, base_rev } => {
                match lib.put(&kind, &doc_id, &title, &body, base_rev, crate::now_ms()) {
                    Ok(saved) => (saved, false),
                    Err(e) => return failed(e),
                }
            }
            Command::DocDelete { kind, id: doc_id, base_rev } => {
                match lib.delete(&kind, &doc_id, base_rev, crate::now_ms()) {
                    Ok(Some(gone)) => (gone, true),
                    Ok(None) => return protocol::ack(id, None),
                    Err(e) => return failed(e),
                }
            }
            _ => unreachable!("only doc commands come here"),
        };
        let (saved, deleted) = changed;
        if let Ok(line) = serde_json::to_vec(&protocol::doc_changed_event(&saved, deleted)) {
            self.send_to_subscribers(line);
        }
        protocol::ack_with(id, None, AckExtras { saved: Some(saved), ..Default::default() })
    }

    /// Sends the current state to every subscriber without ever blocking: a subscriber
    /// whose queue is full is disconnected (it reconnects and resubscribes), and one whose
    /// connection is gone is dropped.
    fn broadcast(&mut self) {
        self.broadcast_at(Instant::now());
    }

    fn broadcast_at(&mut self, now: Instant) {
        let view = self.view_at(now);
        if let Ok(line) = serde_json::to_vec(&protocol::state_event(view.clone())) {
            self.send_to_subscribers(line);
        }
        self.last_sent = Some(view);
    }

    fn send_to_subscribers(&mut self, mut line: Vec<u8>) {
        line.push(b'\n');
        self.subscribers.retain(|s| match s.tx.try_send(line.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                crate::log("engine", "subscriber stopped reading; disconnecting it");
                if let Some(stream) = &s.stream {
                    let _ = stream.shutdown(Shutdown::Both);
                }
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

/// One event to one connection, if it can receive events; never blocks.
fn send_event(outbox: Option<&Outbox>, event: &impl serde::Serialize) {
    if let (Some(o), Ok(mut line)) = (outbox, serde_json::to_vec(event)) {
        line.push(b'\n');
        let _ = o.tx.try_send(line);
    }
}

/// For the log: the command's type, never its arguments.
fn command_name(command: &Command) -> String {
    serde_json::to_value(command).ok().and_then(|v| v["type"].as_str().map(String::from)).unwrap_or_default()
}

/// True if every address `addr` names is on this computer (loopback), so a plain channel on
/// it never crosses the network. Unresolvable or wildcard addresses (`0.0.0.0`) are not.
pub fn is_loopback_only(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(addrs) = addr.to_socket_addrs() else { return false };
    let addrs: Vec<_> = addrs.collect();
    !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback())
}

/// Binds the command channel, retrying briefly in case the previous engine's socket is
/// still being released by the OS after a crash.
pub fn bind(addr: &str, wait: Duration) -> std::io::Result<TcpListener> {
    let start = Instant::now();
    loop {
        match TcpListener::bind(addr) {
            Ok(l) => return Ok(l),
            Err(e) if start.elapsed() < wait => {
                crate::log("engine", format!("bind {addr} failed ({e}); retrying"));
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Accepts connections forever; one thread per connected device.
pub fn serve(listener: TcpListener, engine: Arc<Mutex<Engine>>) {
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let engine = engine.clone();
        std::thread::spawn(move || {
            let _ = serve_connection(stream, &engine);
        });
    }
}

/// Skips input up to and including the next newline (or end of stream) without keeping
/// any of it, so a client that never sends a newline can't grow our memory: only the
/// reader's own fixed-size buffer is ever used. Returns how many bytes were skipped.
fn discard_line(reader: &mut impl BufRead) -> std::io::Result<u64> {
    let mut skipped = 0u64;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(skipped);
        }
        let (used, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        reader.consume(used);
        skipped += used as u64;
        if done {
            return Ok(skipped);
        }
    }
}

fn serve_connection(stream: TcpStream, engine: &Mutex<Engine>) -> std::io::Result<()> {
    // The local channel is trusted, so it only ever takes connections from this computer,
    // whatever address it was told to listen on. Devices use the WebSocket channel.
    let peer = stream.peer_addr()?;
    if !peer.ip().is_loopback() {
        crate::log("engine", format!("refused a local-channel connection from {}", peer.ip()));
        return Ok(());
    }
    stream.set_nodelay(true)?;
    let shared = Arc::new(stream.try_clone()?);
    let (tx, rx) = sync_channel::<Vec<u8>>(OUTBOX_CAPACITY);
    let outbox = Outbox::new(tx, Some(shared.clone()));
    let mut out = stream.try_clone()?;
    std::thread::spawn(move || {
        for msg in rx {
            if out.write_all(&msg).is_err() {
                break;
            }
        }
    });
    let result = read_commands(stream, engine, &outbox);
    // Unblocks the writer and tells the client we're done with this connection.
    let _ = shared.shutdown(Shutdown::Both);
    result
}

fn read_commands(stream: TcpStream, engine: &Mutex<Engine>, outbox: &Outbox) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = (&mut reader).take(MAX_MESSAGE as u64 + 1).read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(());
        }
        let ack = if n > MAX_MESSAGE && buf.last() != Some(&b'\n') {
            discard_line(&mut reader)?;
            protocol::nack("", ErrorCode::BadMessage, "message too long")
        } else {
            // Invalid UTF-8 becomes a bad_message nack, never a dropped connection.
            let line = String::from_utf8_lossy(&buf);
            let msg = line.trim();
            if msg.is_empty() {
                continue;
            }
            // A panic in one handler must not poison the engine for every other device.
            let mut e = engine.lock().unwrap_or_else(|p| p.into_inner());
            e.handle_line_from(msg, Some(outbox))
        };
        let mut bytes = serde_json::to_vec(&ack)?;
        bytes.push(b'\n');
        // Blocking here only stalls this connection, never the engine (the lock is released).
        if outbox.tx.send(bytes).is_err() {
            return Ok(());
        }
    }
}

/// How long a WebSocket connection waits for a message before checking its outbox again:
/// the most an event for a WebSocket device can wait behind a quiet socket.
const WS_POLL: Duration = Duration::from_millis(15);
/// A client that hasn't finished the WebSocket handshake by now is dropped.
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Who may open the WebSocket command channel. Programs (no `Origin` header) always may.
/// Browsers send their page's origin, and only listed origins are accepted, so a web page
/// open on a booth computer can't drive the service. Pages this daemon served itself (the
/// same scheme, host and port) are its own app and may.
#[derive(Debug, Clone, Default)]
pub struct WsAccess {
    pub origins: Vec<String>,
    /// The built web app (`apps/web/build`), served on the same port; None serves no pages.
    pub web_dir: Option<std::path::PathBuf>,
}

impl WsAccess {
    pub fn allows(&self, origin: Option<&str>) -> bool {
        origin.is_none_or(|o| self.origins.iter().any(|a| a.eq_ignore_ascii_case(o)))
    }

    /// Also lets in a page from this very address: `host` is the request's Host header.
    fn allows_from(&self, origin: Option<&str>, host: Option<&str>, secure: bool) -> bool {
        let scheme = if secure { "https" } else { "http" };
        let own = matches!((origin, host), (Some(o), Some(h)) if o.eq_ignore_ascii_case(&format!("{scheme}://{h}")));
        own || self.allows(origin)
    }
}

/// Serves the same protocol over WebSocket: one envelope per text message in, and the same
/// acks and events out, one per text message (the TCP channel's lines, without the newline).
/// With `tls`, every connection is `wss://` with the church's certificate; there is no plain
/// fallback.
pub fn serve_ws(listener: TcpListener, engine: Arc<Mutex<Engine>>, access: Arc<WsAccess>, tls: Option<Arc<TlsFiles>>) {
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (engine, access, tls) = (engine.clone(), access.clone(), tls.clone());
        std::thread::spawn(move || {
            let _ = serve_ws_connection(stream, &engine, &access, tls.as_deref());
        });
    }
}

fn serve_ws_connection(
    stream: TcpStream,
    engine: &Mutex<Engine>,
    access: &WsAccess,
    tls: Option<&TlsFiles>,
) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(WS_HANDSHAKE_TIMEOUT))?;
    let shared = Arc::new(stream.try_clone()?);
    let mut session = Session::device();
    let result = match tls {
        None => ws_session(stream, &shared, engine, access, &mut session, false),
        // No good certificate yet: refuse rather than serve in plain text.
        Some(tls) => match tls.config() {
            Some(config) => {
                let conn = rustls::ServerConnection::new(config).map_err(std::io::Error::other)?;
                ws_session(rustls::StreamOwned::new(conn, stream), &shared, engine, access, &mut session, true)
            }
            None => Ok(()),
        },
    };
    engine.lock().unwrap_or_else(|p| p.into_inner()).forget(&session);
    let _ = shared.shutdown(Shutdown::Both);
    result
}

/// The socket under a WebSocket's stream, plain or TLS.
trait OverTcp: Read + Write {
    fn tcp(&self) -> &TcpStream;
}

impl OverTcp for TcpStream {
    fn tcp(&self) -> &TcpStream {
        self
    }
}

impl OverTcp for rustls::StreamOwned<rustls::ServerConnection, TcpStream> {
    fn tcp(&self) -> &TcpStream {
        self.get_ref()
    }
}

impl<S: OverTcp> OverTcp for crate::web::Replay<S> {
    fn tcp(&self) -> &TcpStream {
        self.inner.tcp()
    }
}

/// One WebSocket connection over `stream` (a plain socket, or TLS over it); `socket` is a
/// clone of that socket, for the outbox to cut a stuck connection.
fn ws_session<S: OverTcp>(
    stream: S,
    socket: &Arc<TcpStream>,
    engine: &Mutex<Engine>,
    access: &WsAccess,
    session: &mut Session,
    secure: bool,
) -> std::io::Result<()> {
    use tungstenite::{Error, Message, handshake::server::ErrorResponse, http::StatusCode};

    // A plain request gets a file of the web app; one asking to upgrade is the channel.
    let mut stream = stream;
    let Some(raw) = crate::web::read_head(&mut stream)? else { return Ok(()) };
    let Some(head) = crate::web::parse(&raw) else { return Ok(()) };
    if !head.upgrade {
        return crate::web::respond(&mut stream, access.web_dir.as_deref(), &head);
    }
    let stream = crate::web::Replay { head: std::io::Cursor::new(raw), inner: stream };

    // The error type is tungstenite's handshake `Callback` contract, so it can't be boxed.
    #[allow(clippy::result_large_err)]
    let check_origin = |req: &tungstenite::handshake::server::Request, resp| {
        let origin = req.headers().get("origin").map(|o| o.to_str().unwrap_or("?"));
        let host = req.headers().get("host").and_then(|h| h.to_str().ok());
        if access.allows_from(origin, host, secure) {
            return Ok(resp);
        }
        crate::log("engine", format!("refused a WebSocket from origin {}", origin.unwrap_or("")));
        let mut no = ErrorResponse::new(Some("this origin may not control the service".into()));
        *no.status_mut() = StatusCode::FORBIDDEN;
        Err(no)
    };
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let Ok(mut ws) = tungstenite::accept_hdr_with_config(stream, check_origin, Some(config)) else {
        return Ok(());
    };
    // On the handle being read: on Windows a clone's timeout doesn't apply to it.
    ws.get_ref().tcp().set_read_timeout(Some(WS_POLL))?;

    let (tx, rx) = sync_channel::<Vec<u8>>(OUTBOX_CAPACITY);
    let outbox = Outbox::new(tx, Some(socket.clone()));
    let as_message = |mut bytes: Vec<u8>| {
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        Message::text(String::from_utf8_lossy(&bytes).into_owned())
    };
    loop {
        let ack = match ws.read() {
            Ok(Message::Text(text)) if text.trim().is_empty() => None,
            Ok(Message::Text(text)) => {
                // A panic in one handler must not poison the engine for every other device.
                let mut e = engine.lock().unwrap_or_else(|p| p.into_inner());
                Some(e.handle(text.trim(), session, Some(&outbox)))
            }
            Ok(Message::Binary(_)) => Some(protocol::nack("", ErrorCode::BadMessage, "send JSON as text messages")),
            // tungstenite has queued the close reply; the flush below sends it, and the next
            // read reports the connection closed, so the client sees a clean close.
            Ok(Message::Close(_)) => None,
            Ok(_) => None, // ping, pong: tungstenite answers pings itself
            Err(Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                None
            }
            Err(Error::Capacity(_)) => {
                let _ = ws.send(as_message(serde_json::to_vec(&protocol::nack(
                    "",
                    ErrorCode::BadMessage,
                    "message too long",
                ))?));
                break Ok(());
            }
            Err(_) => break Ok(()),
        };
        // Events first, then the ack: the same order the TCP channel writes them in.
        let mut sent = Ok(());
        while let Ok(event) = rx.try_recv() {
            sent = sent.and_then(|_| ws.write(as_message(event)));
        }
        if let Some(ack) = ack {
            let bytes = serde_json::to_vec(&ack)?;
            sent = sent.and_then(|_| ws.write(as_message(bytes)));
        }
        // Flush even if a write failed (e.g. an event after the client closed), so a queued
        // close reply still goes out.
        let flushed = ws.flush();
        match sent.and(flushed) {
            Ok(()) => {}
            Err(Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn engine(name: &str, slides: u64) -> (Engine, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("jivvy-engine-{name}-{}-{}", std::process::id(), crate::now_ms()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        (Engine::new(store, snap, slides), dir)
    }

    fn send(e: &mut Engine, command: Value) -> Value {
        let msg = json!({ "v": 1, "id": "t", "ts": 1, "command": command }).to_string();
        serde_json::to_value(e.handle_line(&msg)).unwrap()
    }

    #[test]
    fn the_library_keeps_songs_across_a_restart_and_every_screen_hears_of_changes() {
        let (mut e, dir) = engine("library", 10);
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        let subscribe = r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#;
        e.handle_line_from(subscribe, Some(&Outbox::new(tx, None)));
        while rx.try_recv().is_ok() {}

        let song =
            json!({ "sections": [{ "id": "v1", "label": "Verse 1", "text": "Amazing grace" }], "arrangement": ["v1"] });
        let put = json!({ "type": "doc.put", "kind": "song", "id": "grace", "title": "Amazing Grace", "body": song, "baseRev": 0 });
        assert_eq!(send(&mut e, put.clone())["saved"]["rev"], 1);
        let ev: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!((ev["event"].clone(), ev["doc"]["id"].clone()), (json!("doc.changed"), json!("grace")));
        // Saving again from the same old copy (revision 0) would lose the first save: refused.
        assert_eq!(send(&mut e, put)["code"], "conflict");

        // A restart (a new engine on the same data) still has it.
        drop(e);
        let mut e = Engine::new(Store::new(&dir).unwrap(), Snapshot::default(), 10);
        assert_eq!(send(&mut e, json!({ "type": "doc.list", "kind": "song" }))["docs"][0]["title"], "Amazing Grace");
        assert_eq!(send(&mut e, json!({ "type": "doc.get", "kind": "song", "id": "grace" }))["doc"]["body"], song);
    }

    #[test]
    fn a_run_sheet_drives_the_slides_and_survives_a_restart() {
        let (mut e, dir) = engine("runsheet", 100);
        let sheet = json!({ "id": "sun", "title": "Sunday", "items": [
            { "id": "s1", "kind": "song", "title": "Amazing Grace",
              "slides": [{ "lines": ["Amazing grace"] }, { "lines": ["I once was lost"] }] },
            { "id": "c1", "kind": "countdown", "title": "Countdown",
              "slides": [{ "lines": ["Starts in"], "countdownTo": 1_760_000_000_000u64 }] },
        ] });
        let r = send(&mut e, json!({ "type": "runsheet.set", "runSheet": sheet }));
        assert_eq!(
            (r["state"]["slideCount"].clone(), r["state"]["runSheetRev"].clone(), r["state"]["slide"].clone()),
            (json!(3), json!(1), json!({ "lines": ["Amazing grace"] }))
        );
        for _ in 0..5 {
            send(&mut e, json!({ "type": "slide.next" }));
        }
        assert_eq!(e.view().slide_index, 2, "clamped to the run sheet's last slide");
        let r = send(&mut e, json!({ "type": "slide.prev" }));
        assert_eq!(r["state"]["slide"]["lines"], json!(["I once was lost"]));

        drop(e);
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let mut e = Engine::new(store, snap, 100);
        let r = send(&mut e, json!({ "type": "runsheet.get" }));
        assert_eq!((r["runSheet"].clone(), r["state"]["slideIndex"].clone()), (sheet, json!(1)));

        // A crash after a new run sheet was saved but before the snapshot: it starts at the top.
        drop(e);
        let other: protocol::RunSheet = serde_json::from_value(json!({ "id": "wed", "title": "", "items": [
            { "id": "t", "kind": "text", "title": "", "slides": [{ "lines": ["a"] }, { "lines": ["b"] }] },
        ] }))
        .unwrap();
        crate::runsheet::save(&dir, &crate::runsheet::Saved { version: 1, rev: 2, run_sheet: other }).unwrap();
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        assert_eq!(Engine::new(store, snap, 100).view().slide_index, 0);
    }

    #[test]
    fn a_looping_item_moves_on_by_itself_and_wraps_until_the_operator_leaves_it() {
        let (mut e, _) = engine("loop", 100);
        let sheet = json!({ "id": "sun", "title": "", "items": [
            { "id": "pre", "kind": "loop", "title": "Welcome", "advanceSeconds": 5,
              "slides": [{ "lines": ["Welcome"] }, { "lines": ["Coffee"] }] },
            { "id": "s1", "kind": "song", "title": "", "slides": [{ "lines": ["Amazing grace"] }] },
        ] });
        // The daemon has sat on slide 1 for a while before the run sheet arrives.
        let before = Instant::now();
        e.tick_at(before);
        e.tick_at(before + Duration::from_secs(60));
        send(&mut e, json!({ "type": "runsheet.set", "runSheet": sheet }));
        let t0 = before + Duration::from_secs(61);
        e.tick_at(t0);
        assert_eq!(e.view().slide_index, 0, "the loop's first slide gets its full time");
        e.tick_at(t0 + Duration::from_secs(4));
        assert_eq!(e.view().slide_index, 0, "not yet");
        e.tick_at(t0 + Duration::from_secs(5));
        assert_eq!(e.view().slide_index, 1);
        e.tick_at(t0 + Duration::from_secs(10));
        assert_eq!(e.view().slide_index, 0, "back to the loop's first slide");
        // Away and straight back between two ticks: the slide gets its full time again.
        e.tick_at(t0 + Duration::from_secs(14));
        send(&mut e, json!({ "type": "slide.goto", "index": 1 }));
        send(&mut e, json!({ "type": "slide.goto", "index": 0 }));
        e.tick_at(t0 + Duration::from_secs(15));
        assert_eq!(e.view().slide_index, 0, "not moved on at once");
        let r = send(&mut e, json!({ "type": "slide.next" }));
        assert_eq!(r["state"]["slideIndex"], 2, "Next leaves the loop for the song");
        e.tick_at(t0 + Duration::from_secs(60));
        assert_eq!(e.view().slide_index, 2, "the song waits for the operator");
    }

    #[test]
    fn next_stops_a_loop_at_the_end_of_the_run_sheet() {
        let (mut e, dir) = engine("loop-end", 100);
        let sheet = json!({ "id": "pre", "title": "", "items": [
            { "id": "s1", "kind": "song", "title": "", "slides": [{ "lines": ["Amazing grace"] }] },
            { "id": "post", "kind": "loop", "title": "", "advanceSeconds": 5,
              "slides": [{ "lines": ["Thanks for coming"] }, { "lines": ["See you next week"] }] },
        ] });
        send(&mut e, json!({ "type": "runsheet.set", "runSheet": sheet }));
        send(&mut e, json!({ "type": "slide.goto", "index": 1 }));
        let t0 = Instant::now();
        e.tick_at(t0);
        send(&mut e, json!({ "type": "slide.next" }));
        e.tick_at(t0 + Duration::from_secs(60));
        assert_eq!(e.view().slide_index, 2, "on the loop's last slide, and it stays there");
        // Still stopped after the engine restarts.
        drop(e);
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let mut e = Engine::new(store, snap, 100);
        e.tick_at(t0);
        e.tick_at(t0 + Duration::from_secs(60));
        assert_eq!(e.view().slide_index, 2, "a restart doesn't start the loop again");
        send(&mut e, json!({ "type": "slide.goto", "index": 1 }));
        e.tick_at(t0 + Duration::from_secs(61));
        e.tick_at(t0 + Duration::from_secs(66));
        assert_eq!(e.view().slide_index, 2, "going back into it loops again");
        // Next on the loop's last slide stops it too.
        send(&mut e, json!({ "type": "slide.next" }));
        e.tick_at(t0 + Duration::from_secs(120));
        assert_eq!(e.view().slide_index, 2, "Next on the last slide holds it");

        // A crash after another run sheet was saved: its loop isn't held by the old stop.
        drop(e);
        let other: protocol::RunSheet = serde_json::from_value(json!({ "id": "next", "title": "", "items": [
            { "id": "pre", "kind": "loop", "title": "", "advanceSeconds": 5,
              "slides": [{ "lines": ["Welcome"] }, { "lines": ["Coffee"] }] },
        ] }))
        .unwrap();
        crate::runsheet::save(&dir, &crate::runsheet::Saved { version: 1, rev: 9, run_sheet: other }).unwrap();
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let mut e = Engine::new(store, snap, 100);
        e.tick_at(t0);
        e.tick_at(t0 + Duration::from_secs(5));
        assert_eq!(e.view().slide_index, 1, "the new run sheet's loop runs");
    }

    #[test]
    fn an_image_sent_in_pieces_can_be_shown_on_a_slide() {
        use base64::Engine as _;
        let (mut e, dir) = engine("image", 100);
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend(std::iter::repeat_n(7u8, 300_000));
        let id: String =
            ring::digest::digest(&ring::digest::SHA256, &png).as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let sheet = json!({ "id": "img", "title": "", "items": [
            { "id": "i", "kind": "image", "title": "", "slides": [{ "lines": [], "image": id }] },
        ] });
        let r = send(&mut e, json!({ "type": "runsheet.set", "runSheet": sheet }));
        assert_eq!(r["code"], "bad_arguments", "refused until the image is here");
        assert_eq!(send(&mut e, json!({ "type": "media.has", "ids": [id] }))["missing"], json!([id]));

        let (a, b) = png.split_at(200_000);
        let r = send(&mut e, json!({ "type": "media.put", "id": id, "offset": 0, "data": b64(a), "last": false }));
        assert_eq!(r["media"], json!({ "id": id, "received": 200_000, "complete": false }));
        let r = send(&mut e, json!({ "type": "media.put", "id": id, "offset": 200_000, "data": b64(b), "last": true }));
        assert_eq!(r["media"]["complete"], true);
        assert_eq!(std::fs::read(crate::images::path(&dir, &id)).unwrap(), png);
        assert_eq!(send(&mut e, json!({ "type": "media.has", "ids": [id] }))["missing"], json!([]));

        let r = send(&mut e, json!({ "type": "runsheet.set", "runSheet": sheet }));
        assert_eq!(r["state"]["slide"], json!({ "lines": [], "image": id }));
        // A clean-up started for an earlier run sheet (which didn't show it) and still running
        // checks what is protected now, so it leaves the image this run sheet shows.
        e.wait_for_pruning();
        assert_eq!(crate::images::prune(&dir, &e.protected, &Default::default()), 0);
        assert!(crate::images::has(&dir, &id));

        // The next run sheet doesn't show it: the church computer doesn't keep it.
        let next = json!({ "id": "next", "title": "", "items": [] });
        send(&mut e, json!({ "type": "runsheet.set", "runSheet": next }));
        e.wait_for_pruning();
        assert!(!crate::images::has(&dir, &id));
    }

    #[test]
    fn navigates_and_clamps_like_the_simulated_daemon() {
        let (mut e, _) = engine("nav", 5);
        send(&mut e, json!({ "type": "slide.prev" }));
        assert_eq!(e.state().slide_index, 0);
        for _ in 0..10 {
            send(&mut e, json!({ "type": "slide.next" }));
        }
        assert_eq!(e.state().slide_index, 4);
        let r = send(&mut e, json!({ "type": "slide.goto", "index": 9 }));
        assert_eq!((r["ok"].clone(), r["code"].clone()), (json!(false), json!("bad_arguments")));
    }

    #[test]
    fn every_ack_reports_state_and_changes_are_saved_before_the_ack() {
        let (mut e, dir) = engine("save", 10);
        let r = send(&mut e, json!({ "type": "slide.goto", "index": 7 }));
        assert_eq!(
            r["state"],
            json!({
                "slideIndex": 7, "black": false, "stream": "off", "streamWanted": false,
                "recording": "off", "recordingWanted": false, "recordingLowDisk": false
            })
        );
        send(&mut e, json!({ "type": "output.black", "on": true }));
        let (snap, _) = Store::new(&dir).unwrap().load();
        assert_eq!((snap.slide_index, snap.black), (7, true));
        let r = send(&mut e, json!({ "type": "state.get" }));
        assert_eq!(r["state"]["black"], json!(true));
    }

    #[test]
    fn a_new_engine_resumes_from_the_saved_snapshot() {
        let (mut e, dir) = engine("resume", 10);
        send(&mut e, json!({ "type": "slide.goto", "index": 6 }));
        drop(e);
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        assert_eq!(Engine::new(store, snap, 10).state().slide_index, 6);
    }

    #[test]
    fn a_snapshot_past_the_end_of_a_shorter_run_sheet_is_clamped() {
        let (mut e, dir) = engine("clamp", 10);
        send(&mut e, json!({ "type": "slide.goto", "index": 9 }));
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        assert_eq!(Engine::new(store, snap, 4).state().slide_index, 3);
    }

    #[test]
    fn the_stream_is_live_only_while_reports_keep_coming_and_wanted_survives_a_restart() {
        let (mut e, dir) = engine("stream", 3);
        let stream = |e: &mut Engine| send(e, json!({ "type": "state.get" }))["state"].clone();
        assert_eq!(stream(&mut e)["stream"], json!("off"));
        send(&mut e, json!({ "type": "stream.start" }));
        let s = stream(&mut e);
        assert_eq!((s["stream"].clone(), s["streamWanted"].clone()), (json!("reconnecting"), json!(true)));
        send(&mut e, json!({ "type": "stream.report", "status": "live" }));
        assert_eq!(stream(&mut e)["stream"], json!("live"));
        let later = Instant::now() + STREAM_REPORT_STALE + Duration::from_millis(1);
        assert_eq!(e.view_at(later).stream, StreamStatus::Reconnecting, "a silent video process is never shown live");
        send(&mut e, json!({ "type": "stream.report", "status": "reconnecting" }));
        assert_eq!(stream(&mut e)["stream"], json!("reconnecting"));

        // Wanted is saved; the live report is not.
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let mut restarted = Engine::new(store, snap, 3);
        assert!(restarted.view().stream_wanted);
        assert_eq!(restarted.view().stream, StreamStatus::Reconnecting);
        send(&mut restarted, json!({ "type": "stream.stop" }));
        assert_eq!(restarted.view().stream, StreamStatus::Off);
    }

    #[test]
    fn the_recording_is_shown_only_while_reports_keep_coming_and_wanted_survives_a_restart() {
        let (mut e, dir) = engine("recording", 3);
        let view = |e: &mut Engine| send(e, json!({ "type": "state.get" }))["state"].clone();
        let report = |e: &mut Engine, status: &str, low: bool| {
            send(e, json!({ "type": "recording.report", "status": status, "lowDisk": low }))
        };
        assert_eq!(view(&mut e)["recording"], json!("off"));
        // A report with nothing asked for changes nothing: no recording, no warning.
        report(&mut e, "recording", true);
        let v = view(&mut e);
        assert_eq!((v["recording"].clone(), v["recordingLowDisk"].clone()), (json!("off"), json!(false)));

        send(&mut e, json!({ "type": "recording.start" }));
        let v = view(&mut e);
        assert_eq!((v["recording"].clone(), v["recordingWanted"].clone()), (json!("recording"), json!(true)));
        let later = Instant::now() + RECORDING_REPORT_STALE + Duration::from_millis(1);
        let silent = e.view_at(later);
        assert_eq!(silent.recording, RecordingStatus::Waiting, "a silent video process is never shown recording");
        assert!(!silent.recording_low_disk, "and its old warning goes with it");
        report(&mut e, "waiting", false);
        assert_eq!(view(&mut e)["recording"], json!("waiting"));
        report(&mut e, "recording", false);
        assert_eq!(view(&mut e)["recordingLowDisk"], json!(false));

        // Wanted is saved; the live report is not.
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let mut restarted = Engine::new(store, snap, 3);
        assert!(restarted.view().recording_wanted);
        assert_eq!(restarted.view().recording, RecordingStatus::Waiting, "until the video process says otherwise");
        send(&mut restarted, json!({ "type": "recording.stop" }));
        assert_eq!(restarted.view().recording, RecordingStatus::Off);
        assert!(!restarted.view().recording_wanted);
    }

    #[test]
    fn a_state_saved_by_an_older_daemon_still_loads_with_the_recording_off() {
        let (mut e, dir) = engine("old-state", 3);
        send(&mut e, json!({ "type": "slide.goto", "index": 2 }));
        let store = Store::new(&dir).unwrap();
        let mut old: Value = serde_json::from_slice(&std::fs::read(store.current_path()).unwrap()).unwrap();
        assert!(old.as_object_mut().unwrap().remove("recordingWanted").is_some());
        std::fs::write(store.current_path(), old.to_string()).unwrap();
        let (snap, source) = store.load();
        assert_eq!((source, snap.slide_index, snap.recording_wanted), (crate::snapshot::Source::Current, 2, false));
    }

    #[test]
    fn the_low_disk_warning_reaches_subscribers_and_clears_when_space_returns() {
        let (mut e, _) = engine("low-disk", 3);
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        e.handle_line_from(
            r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#,
            Some(&Outbox::new(tx, None)),
        );
        send(&mut e, json!({ "type": "recording.start" }));
        send(&mut e, json!({ "type": "recording.report", "status": "recording", "lowDisk": true }));
        let events: Vec<Value> = rx.try_iter().map(|b| serde_json::from_slice(&b).unwrap()).collect();
        let last = &events.last().unwrap()["state"];
        assert_eq!((last["recording"].clone(), last["recordingLowDisk"].clone()), (json!("recording"), json!(true)));
        send(&mut e, json!({ "type": "recording.report", "status": "recording", "lowDisk": false }));
        let ev: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(ev["state"]["recordingLowDisk"], json!(false));
    }

    #[test]
    fn subscribers_hear_when_a_stream_report_goes_stale() {
        let (mut e, _) = engine("stale", 3);
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        e.handle_line_from(
            r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#,
            Some(&Outbox::new(tx, None)),
        );
        send(&mut e, json!({ "type": "stream.start" }));
        send(&mut e, json!({ "type": "stream.report", "status": "live" }));
        let events: Vec<Value> = rx.try_iter().map(|b| serde_json::from_slice(&b).unwrap()).collect();
        assert_eq!(events.last().unwrap()["state"]["stream"], json!("live"));
        e.tick(); // nothing changed: nothing sent
        assert!(rx.try_recv().is_err());
        e.tick_at(Instant::now() + STREAM_REPORT_STALE + Duration::from_millis(1));
        let ev: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(ev["state"]["stream"], json!("reconnecting"));
    }

    #[test]
    fn bad_input_is_nacked() {
        let (mut e, _) = engine("bad", 3);
        assert_eq!(serde_json::to_value(e.handle_line("{")).unwrap()["code"], json!("bad_message"));
        let r =
            serde_json::to_value(e.handle_line(r#"{"v":9,"id":"z","ts":1,"command":{"type":"slide.next"}}"#)).unwrap();
        assert_eq!((r["code"].clone(), r["id"].clone()), (json!("unsupported_version"), json!("z")));
    }

    /// Yields `len` bytes of 'x' without allocating them, then `tail`.
    struct Flood {
        len: u64,
        tail: std::io::Cursor<Vec<u8>>,
    }

    impl Read for Flood {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.len == 0 {
                return self.tail.read(out);
            }
            let n = out.len().min(self.len as usize);
            out[..n].fill(b'x');
            self.len -= n as u64;
            Ok(n)
        }
    }

    #[test]
    fn discarding_a_huge_line_keeps_nothing_and_the_next_message_still_reads() {
        let flood = 256 * 1024 * 1024; // would be a 256 MiB allocation if it were retained
        let tail = b"\n{\"next\":1}\n".to_vec();
        let mut r = BufReader::with_capacity(8 * 1024, Flood { len: flood, tail: std::io::Cursor::new(tail) });
        assert_eq!(discard_line(&mut r).unwrap(), flood + 1);
        let mut next = String::new();
        r.read_line(&mut next).unwrap();
        assert_eq!(next, "{\"next\":1}\n");
        // A stream that ends without a newline just stops.
        let mut r = BufReader::new(Flood { len: 100_000, tail: std::io::Cursor::new(Vec::new()) });
        assert_eq!(discard_line(&mut r).unwrap(), 100_000);
    }

    #[test]
    fn subscribers_get_every_change_and_stuck_or_gone_ones_never_block_the_engine() {
        let (mut e, _) = engine("subs", 1000);
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        let sub = |e: &mut Engine, outbox: &Outbox| {
            e.handle_line_from(r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#, Some(outbox))
        };
        assert_eq!(serde_json::to_value(sub(&mut e, &Outbox::new(tx, None))).unwrap()["ok"], json!(true));
        send(&mut e, json!({ "type": "slide.goto", "index": 5 }));
        send(&mut e, json!({ "type": "state.get" })); // no change, no event
        let ev: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(
            ev,
            json!({ "v": 1, "event": "state", "state": {
                "slideIndex": 5, "black": false, "stream": "off", "streamWanted": false,
                "recording": "off", "recordingWanted": false, "recordingLowDisk": false
            } })
        );
        assert!(rx.try_recv().is_err());

        // Never reads: more changes than its queue holds, and the engine carries on.
        for i in 0..(OUTBOX_CAPACITY as u64 + 10) {
            send(&mut e, json!({ "type": "slide.goto", "index": i % 900 }));
        }
        assert_eq!(e.subscriber_count(), 0);
        drop(rx);

        // A subscriber whose connection is gone is dropped on the next change.
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        sub(&mut e, &Outbox::new(tx, None));
        drop(rx);
        send(&mut e, json!({ "type": "slide.next" }));
        assert_eq!(e.subscriber_count(), 0);

        // Without a connection there's nowhere to send events.
        assert_eq!(send(&mut e, json!({ "type": "state.subscribe" }))["code"], json!("unavailable"));
    }

    #[test]
    fn levels_are_relayed_to_subscribers_without_touching_the_saved_state() {
        let (mut e, dir) = engine("levels", 10);
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        e.handle_line_from(
            r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#,
            Some(&Outbox::new(tx, None)),
        );
        let r = send(&mut e, json!({ "type": "media.levels", "peakDb": [-6.0, -7.5], "rmsDb": [-18.0, -20.0] }));
        assert_eq!(r["ok"], json!(true));
        let ev: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(
            ev,
            json!({ "v": 1, "event": "levels", "levels": { "peakDb": [-6.0, -7.5], "rmsDb": [-18.0, -20.0] } })
        );
        assert!(!dir.join("state.json").exists(), "levels must never be written to disk");
    }

    #[test]
    fn a_subscribed_connection_receives_changes_made_by_another_device() {
        let (e, _) = engine("tcp-subs", 10);
        let listener = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || serve(listener, Arc::new(Mutex::new(e))));
        let mut screen = TcpStream::connect(addr).unwrap();
        screen.write_all(b"{\"v\":1,\"id\":\"s\",\"ts\":1,\"command\":{\"type\":\"state.subscribe\"}}\n").unwrap();
        let mut screen = BufReader::new(screen);
        let mut line = String::new();
        screen.read_line(&mut line).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], json!("s"));

        let mut remote = TcpStream::connect(addr).unwrap();
        remote
            .write_all(b"{\"v\":1,\"id\":\"r\",\"ts\":1,\"command\":{\"type\":\"slide.goto\",\"index\":3}}\n")
            .unwrap();
        line.clear();
        screen.read_line(&mut line).unwrap();
        let ev: Value = serde_json::from_str(&line).unwrap();
        assert_eq!((ev["event"].clone(), ev["state"]["slideIndex"].clone()), (json!("state"), json!(3)));
    }

    #[test]
    fn serves_newline_delimited_json_over_tcp() {
        let (e, _) = engine("tcp", 5);
        let listener = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Mutex::new(e));
        std::thread::spawn(move || serve(listener, engine));
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"{\"v\":1,\"id\":\"a\",\"ts\":1,\"command\":{\"type\":\"slide.next\"}}\n\n").unwrap();
        s.write_all(&vec![b'x'; MAX_MESSAGE + 10]).unwrap();
        s.write_all(b"\n{\"v\":1,\"id\":\"b\",\"ts\":1,\"command\":{\"type\":\"state.get\"}}\n").unwrap();
        // A line the size of a real run sheet (well past the old 16 KiB cap) is read whole.
        let pad = "p".repeat(64 * 1024);
        let big =
            format!("{{\"v\":1,\"id\":\"c\",\"ts\":1,\"pad\":\"{pad}\",\"command\":{{\"type\":\"state.get\"}}}}\n");
        s.write_all(big.as_bytes()).unwrap();
        let mut r = BufReader::new(s);
        let mut lines = Vec::new();
        for _ in 0..4 {
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            lines.push(serde_json::from_str::<Value>(&l).unwrap());
        }
        assert_eq!(lines[0]["state"]["slideIndex"], json!(1));
        assert_eq!(lines[1]["message"], json!("message too long"));
        assert_eq!((lines[2]["id"].clone(), lines[2]["state"]["slideIndex"].clone()), (json!("b"), json!(1)));
        assert_eq!((lines[3]["id"].clone(), lines[3]["ok"].clone()), (json!("c"), json!(true)));
    }

    type Ws = tungstenite::WebSocket<TcpStream>;

    /// One engine on both channels: (TCP address, WebSocket address).
    fn serve_both(name: &str, origins: &[&str]) -> (std::net::SocketAddr, std::net::SocketAddr) {
        let (e, _) = engine(name, 10);
        let engine = Arc::new(Mutex::new(e));
        let tcp = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let ws = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addrs = (tcp.local_addr().unwrap(), ws.local_addr().unwrap());
        let access = Arc::new(WsAccess { origins: origins.iter().map(|o| o.to_string()).collect(), web_dir: None });
        let e2 = engine.clone();
        std::thread::spawn(move || serve(tcp, e2));
        std::thread::spawn(move || serve_ws(ws, engine, access, None));
        addrs
    }

    fn ws_connect(addr: std::net::SocketAddr, origin: Option<&str>) -> Result<Ws, tungstenite::Error> {
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/").into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        match tungstenite::client(req, stream) {
            Ok((ws, _)) => Ok(ws),
            Err(tungstenite::HandshakeError::Failure(e)) => Err(e),
            Err(e) => panic!("handshake: {e}"),
        }
    }

    fn ws_send<S: Read + Write>(ws: &mut tungstenite::WebSocket<S>, text: &str) {
        ws.send(tungstenite::Message::text(text.to_string())).unwrap();
    }

    fn ws_next<S: Read + Write>(ws: &mut tungstenite::WebSocket<S>) -> Value {
        loop {
            match ws.read().unwrap() {
                tungstenite::Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                _ => continue,
            }
        }
    }

    fn envelope(command: Value) -> String {
        json!({ "v": 1, "id": "t", "ts": 1, "command": command }).to_string()
    }

    /// Sends one command and returns the next message that isn't an event.
    fn ws_cmd<S: Read + Write>(ws: &mut tungstenite::WebSocket<S>, command: Value) -> Value {
        ws_send(ws, &envelope(command));
        loop {
            let m = ws_next(ws);
            if m.get("event").is_none() {
                return m;
            }
        }
    }

    /// Proves `key` on this connection, as a paired device does on every connection.
    fn prove<S: Read + Write>(ws: &mut tungstenite::WebSocket<S>, key: &TestKey) -> Value {
        let hello = ws_cmd(ws, json!({ "type": "auth.hello", "publicKey": key.public() }));
        let message = protocol::auth_message(hello["daemonId"].as_str().unwrap(), hello["challenge"].as_str().unwrap());
        ws_cmd(ws, json!({ "type": "auth.prove", "signature": key.sign(&message) }))
    }

    fn tcp_ask(addr: std::net::SocketAddr, line: &str) -> Value {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("{line}\n").as_bytes()).unwrap();
        let mut l = String::new();
        BufReader::new(s).read_line(&mut l).unwrap();
        serde_json::from_str(&l).unwrap()
    }

    #[test]
    fn websocket_carries_the_same_envelopes_acks_and_events_as_tcp() {
        let (tcp, ws_addr) = serve_both("ws-same", &[]);
        let mut ws = ws_connect(ws_addr, None).unwrap();
        pair_over_the_wire(tcp, &mut ws, &TestKey::new());
        let goto = r#"{"v":1,"id":"g","ts":1,"command":{"type":"slide.goto","index":3}}"#;
        ws_send(&mut ws, goto);
        let ws_ack = ws_next(&mut ws);
        let get = r#"{"v":1,"id":"g","ts":1,"command":{"type":"state.get"}}"#;
        assert_eq!(ws_ack, tcp_ask(tcp, get), "the same ack, byte for byte, on either channel");

        // Subscribed over WebSocket, it hears a change made over TCP.
        ws_send(&mut ws, r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#);
        assert_eq!(ws_next(&mut ws)["id"], "s");
        tcp_ask(tcp, r#"{"v":1,"id":"n","ts":1,"command":{"type":"slide.next"}}"#);
        let event = ws_next(&mut ws);
        assert_eq!((event["event"].clone(), event["state"]["slideIndex"].clone()), (json!("state"), json!(4)));

        // A bad envelope is nacked the same way, and the connection stays up.
        ws_send(&mut ws, "{not json");
        assert_eq!(ws_next(&mut ws), tcp_ask(tcp, "{not json"));
        ws.send(tungstenite::Message::binary(vec![1u8, 2, 3])).unwrap();
        assert_eq!(ws_next(&mut ws)["code"], "bad_message");
        ws_send(&mut ws, get);
        assert_eq!(ws_next(&mut ws)["state"]["slideIndex"], 4);
    }

    #[test]
    fn browsers_may_connect_only_from_listed_origins() {
        let (_, ws_addr) = serve_both("ws-origin", &["https://app.jivvy.org"]);
        assert!(ws_connect(ws_addr, None).is_ok(), "programs send no Origin");
        assert!(ws_connect(ws_addr, Some("https://app.jivvy.org")).is_ok());
        match ws_connect(ws_addr, Some("https://evil.example")) {
            Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 403),
            other => panic!("an unlisted origin is refused, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn the_web_app_is_served_on_the_channel_port_and_its_pages_may_connect() {
        let web = std::env::temp_dir().join(format!("jivvy-webapp-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join("index.html"), "<!doctype html><title>Jivvy Live</title>").unwrap();
        let (e, _) = engine("web-app", 10);
        let listener = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addr = listener.local_addr().unwrap();
        let access = Arc::new(WsAccess { origins: vec![], web_dir: Some(web.clone()) });
        std::thread::spawn(move || serve_ws(listener, Arc::new(Mutex::new(e)), access, None));

        // A browser asks for the page...
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("GET /operate HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes()).unwrap();
        let mut page = String::new();
        s.read_to_string(&mut page).unwrap();
        assert!(page.starts_with("HTTP/1.1 200") && page.ends_with("<title>Jivvy Live</title>"), "{page}");

        // ...and that page, from this very address, may open the channel; another site may not.
        let mut ws = ws_connect(addr, Some(&format!("http://{addr}"))).unwrap();
        ws_send(&mut ws, r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.get"}}"#);
        assert_eq!(ws_next(&mut ws)["id"], "s");
        assert!(ws_connect(addr, Some("http://evil.example")).is_err());
        let _ = std::fs::remove_dir_all(&web);
    }

    #[test]
    fn a_client_that_closes_gets_a_clean_close_back() {
        let (_, ws_addr) = serve_both("ws-close", &[]);
        let mut ws = ws_connect(ws_addr, None).unwrap();
        ws_send(&mut ws, r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#);
        assert_eq!(ws_next(&mut ws)["id"], "s");
        ws.close(None).unwrap();
        // The server's close reply ends it cleanly (ConnectionClosed), not a dropped socket.
        let end = loop {
            match ws.read() {
                Ok(_) => continue,
                Err(e) => break e,
            }
        };
        assert!(matches!(end, tungstenite::Error::ConnectionClosed), "closed uncleanly: {end}");
    }

    #[test]
    fn an_oversized_websocket_message_is_refused() {
        let (_, ws_addr) = serve_both("ws-big", &[]);
        let mut ws = ws_connect(ws_addr, None).unwrap();
        // The engine refuses from the frame header, so it may cut the connection while the
        // client is still writing; either way nothing this big gets through.
        let _ = ws.send(tungstenite::Message::text("x".repeat(MAX_MESSAGE + 10)));
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) => {
                assert_eq!(serde_json::from_str::<Value>(&t).unwrap()["message"], "message too long")
            }
            Ok(other) => panic!("expected the refusal, got {other:?}"),
            Err(_) => {} // cut off mid-write
        }
        // A message under the limit, such as a run sheet, still works on a new connection.
        let mut ws = ws_connect(ws_addr, None).unwrap();
        ws_send(&mut ws, &"x".repeat(64 * 1024));
        assert_eq!(ws_next(&mut ws)["message"], "invalid JSON");
    }

    /// Pairs `key` the way a phone does: the church computer opens pairing and approves the
    /// request (over the local channel), then the device proves its key.
    fn pair_over_the_wire<S: Read + Write>(
        tcp: std::net::SocketAddr,
        ws: &mut tungstenite::WebSocket<S>,
        key: &TestKey,
    ) {
        assert_eq!(tcp_ask(tcp, &envelope(json!({ "type": "pair.open" })))["pairing"]["open"], true);
        let req = ws_cmd(ws, json!({ "type": "pair.request", "publicKey": key.public(), "name": "Test phone" }));
        let request_id = req["request"]["requestId"].as_str().unwrap().to_string();
        tcp_ask(tcp, &envelope(json!({ "type": "pair.approve", "requestId": request_id })));
        let result = ws_next(ws);
        assert_eq!((result["event"].clone(), result["approved"].clone()), (json!("pair.result"), json!(true)));
        assert_eq!(prove(ws, key)["ok"], true);
    }

    type Wss = tungstenite::WebSocket<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>;
    const CHURCH: &str = "church.d.jivvy.org";

    /// An engine serving `wss://` with whatever is in `cert` and `key`, with one device
    /// already paired: its key.
    fn serve_tls(name: &str, cert: &std::path::Path, key: &std::path::Path) -> (std::net::SocketAddr, TestKey) {
        let (mut e, _) = engine(name, 10);
        let device = TestKey::new();
        e.devices.add("Test phone", &device.public(), 1).unwrap();
        let ws = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addr = ws.local_addr().unwrap();
        let tls = Some(Arc::new(TlsFiles::new(cert, key)));
        std::thread::spawn(move || serve_ws(ws, Arc::new(Mutex::new(e)), Arc::new(WsAccess::default()), tls));
        (addr, device)
    }

    /// Connects trusting only `ca_pem`, as a browser trusts only real certificate authorities.
    fn wss_connect(addr: std::net::SocketAddr, ca_pem: &str) -> Result<Wss, String> {
        use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap()).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let conn = rustls::ClientConnection::new(Arc::new(config), ServerName::try_from(CHURCH).unwrap()).unwrap();
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        tungstenite::client(format!("wss://{CHURCH}/"), rustls::StreamOwned::new(conn, stream))
            .map(|(ws, _)| ws)
            .map_err(|e| e.to_string())
    }

    fn wss_ask(ws: &mut Wss, command: &str) -> Value {
        let msg = format!(r#"{{"v":1,"id":"q","ts":1,"command":{{"type":"{command}"}}}}"#);
        ws.send(tungstenite::Message::text(msg)).unwrap();
        loop {
            if let tungstenite::Message::Text(t) = ws.read().unwrap() {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }

    fn write_pair(dir: &std::path::Path, cert: &str, key: &str) {
        std::fs::write(dir.join("cert.pem"), cert).unwrap();
        std::fs::write(dir.join("key.pem"), key).unwrap();
    }

    #[test]
    fn wss_carries_the_protocol_and_never_falls_back_to_plain_ws() {
        let dir = crate::tls::tests::temp("wss");
        let (ca, cert, key) = crate::tls::tests::test_pair(CHURCH);
        write_pair(&dir, &cert, &key);
        let (addr, device) = serve_tls("wss", &dir.join("cert.pem"), &dir.join("key.pem"));

        let mut ws = wss_connect(addr, &ca).unwrap();
        assert_eq!(prove(&mut ws, &device)["ok"], true);
        assert_eq!(wss_ask(&mut ws, "slide.next")["state"]["slideIndex"], 1);
        // Events reach a quiet subscriber over TLS too.
        assert_eq!(wss_ask(&mut ws, "state.subscribe")["ok"], true);
        let mut other = wss_connect(addr, &ca).unwrap();
        prove(&mut other, &device);
        let sent = Instant::now();
        for _ in 0..20 {
            wss_ask(&mut other, "slide.next");
        }
        for want in 2..=11 {
            let event = loop {
                if let tungstenite::Message::Text(t) = ws.read().unwrap() {
                    break serde_json::from_str::<Value>(&t).unwrap();
                }
            };
            assert_eq!(
                (event["event"].clone(), event["state"]["slideIndex"].clone()),
                (json!("state"), json!(want.min(9)))
            );
            if want == 9 {
                break; // clamped at the last slide: no more changes, so no more events
            }
        }
        // WS_POLL bounds the wait; seconds would mean the read timeout isn't on the TLS socket.
        assert!(sent.elapsed() < Duration::from_secs(2), "events took {:?}", sent.elapsed());
        assert!(ws_connect(addr, None).is_err(), "plain ws:// is refused on a wss channel");
        let (other_ca, _, _) = crate::tls::tests::test_pair(CHURCH);
        assert!(wss_connect(addr, &other_ca).is_err(), "a client that doesn't trust the certificate can't connect");
    }

    #[test]
    fn a_renewed_certificate_is_used_without_a_restart_and_open_connections_stay() {
        let dir = crate::tls::tests::temp("wss-renew");
        let (ca, cert, key) = crate::tls::tests::test_pair(CHURCH);
        write_pair(&dir, &cert, &key);
        let (addr, device) = serve_tls("wss-renew", &dir.join("cert.pem"), &dir.join("key.pem"));
        let mut open = wss_connect(addr, &ca).unwrap();
        prove(&mut open, &device);

        let (new_ca, new_cert, new_key) = crate::tls::tests::test_pair(CHURCH);
        write_pair(&dir, &new_cert, &new_key);
        let mut renewed = wss_connect(addr, &new_ca).expect("new connections get the renewed certificate");
        assert!(wss_connect(addr, &ca).is_err(), "the old certificate is no longer served");
        prove(&mut renewed, &device);
        assert_eq!(wss_ask(&mut renewed, "slide.next")["state"]["slideIndex"], 1);
        assert_eq!(wss_ask(&mut open, "state.get")["state"]["slideIndex"], 1, "the open connection carries on");
    }

    #[test]
    fn with_no_certificate_yet_connections_are_refused_until_it_arrives() {
        let dir = crate::tls::tests::temp("wss-later");
        let (addr, device) = serve_tls("wss-later", &dir.join("cert.pem"), &dir.join("key.pem"));
        let (ca, cert, key) = crate::tls::tests::test_pair(CHURCH);
        assert!(wss_connect(addr, &ca).is_err());
        assert!(ws_connect(addr, None).is_err(), "no certificate never means plain text");
        write_pair(&dir, &cert, &key);
        let mut ws = wss_connect(addr, &ca).expect("the certificate is picked up when it arrives");
        prove(&mut ws, &device);
        assert_eq!(wss_ask(&mut ws, "state.get")["ok"], true);
    }

    // ---- Pairing and device authentication ----

    use crate::devices::tests::TestKey;

    /// A device connection in-process: its session, and the events it receives.
    struct Phone {
        session: Session,
        outbox: Outbox,
        events: std::sync::mpsc::Receiver<Vec<u8>>,
    }

    impl Phone {
        fn new() -> Phone {
            let (tx, events) = sync_channel(64);
            Phone { session: Session::device(), outbox: Outbox::new(tx, None), events }
        }
        fn send(&mut self, e: &mut Engine, command: Value) -> Value {
            serde_json::to_value(e.handle(&envelope(command), &mut self.session, Some(&self.outbox))).unwrap()
        }
        fn prove(&mut self, e: &mut Engine, key: &TestKey) -> Value {
            let hello = self.send(e, json!({ "type": "auth.hello", "publicKey": key.public() }));
            let msg = protocol::auth_message(hello["daemonId"].as_str().unwrap(), hello["challenge"].as_str().unwrap());
            self.send(e, json!({ "type": "auth.prove", "signature": key.sign(&msg) }))
        }
        fn events(&self) -> Vec<Value> {
            self.events.try_iter().map(|b| serde_json::from_slice(&b).unwrap()).collect()
        }
    }

    /// Pairs `key` in-process; returns its device id.
    fn pair(e: &mut Engine, key: &TestKey) -> String {
        send(e, json!({ "type": "pair.open" }));
        let mut phone = Phone::new();
        let req = phone.send(e, json!({ "type": "pair.request", "publicKey": key.public(), "name": "Phone" }));
        send(e, json!({ "type": "pair.approve", "requestId": req["request"]["requestId"] }));
        e.forget(&phone.session);
        phone.events().last().unwrap()["deviceId"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_device_pairs_with_a_code_the_church_computer_checks_and_then_runs_the_service() {
        let (mut e, _) = engine("pair-flow", 10);
        let key = TestKey::new();
        let (tx, church_events) = sync_channel(64);
        let church = Outbox::new(tx, None);
        e.handle_line_from(&envelope(json!({ "type": "state.subscribe" })), Some(&church));

        let opened = send(&mut e, json!({ "type": "pair.open" }));
        assert_eq!(opened["pairing"]["open"], true);
        let closes = opened["pairing"]["closesInMs"].as_u64().unwrap();
        assert!((119_000..=120_000).contains(&closes), "{closes}");

        let mut phone = Phone::new();
        let req =
            phone.send(&mut e, json!({ "type": "pair.request", "publicKey": key.public(), "name": "Sam's iPhone" }));
        let code = req["request"]["code"].as_str().unwrap().to_string();
        assert!(code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()));
        // The church computer sees the request with the same code the phone shows.
        let seen: Vec<Value> = church_events.try_iter().map(|b| serde_json::from_slice(&b).unwrap()).collect();
        let pairing = seen.iter().rev().find(|ev| ev["event"] == "pairing").unwrap();
        assert_eq!(pairing["pairing"]["requests"][0]["code"], json!(code));
        assert_eq!(pairing["pairing"]["requests"][0]["name"], "Sam's iPhone");
        // Asking isn't being paired.
        assert_eq!(phone.send(&mut e, json!({ "type": "slide.next" }))["code"], "unauthorized");

        send(&mut e, json!({ "type": "pair.approve", "requestId": req["request"]["requestId"] }));
        let result = phone.events().pop().unwrap();
        assert_eq!((result["event"].clone(), result["approved"].clone()), (json!("pair.result"), json!(true)));
        assert_eq!(result["daemonId"], json!(e.daemon_id()));

        let mut phone = Phone::new();
        let proved = phone.prove(&mut e, &key);
        assert_eq!((proved["ok"].clone(), proved["deviceId"].clone()), (json!(true), result["deviceId"].clone()));
        assert_eq!(phone.send(&mut e, json!({ "type": "slide.next" }))["state"]["slideIndex"], 1);
        let devices = send(&mut e, json!({ "type": "device.list" }));
        assert_eq!(devices["devices"][0]["name"], "Sam's iPhone");
    }

    fn report(e: &mut Engine, destinations: Value) -> Value {
        send(e, json!({ "type": "stream.report", "status": "live", "destinations": destinations }))
    }

    fn alerts(rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<Value> {
        rx.try_iter()
            .map(|b| serde_json::from_slice::<Value>(&b).unwrap())
            .filter(|v| v["event"] == "alert")
            .map(|v| v["alert"].clone())
            .collect()
    }

    #[test]
    fn a_platform_lowered_or_paused_is_an_alert_and_every_screen_sees_each_platform() {
        let (mut e, _) = engine("alerts", 10);
        let (tx, rx) = sync_channel(64);
        e.handle_line_from(&envelope(json!({ "type": "state.subscribe" })), Some(&Outbox::new(tx, None)));
        let yt = |state: &str, q: &str| json!({ "id": "yt", "name": "YouTube", "state": state, "quality": q });
        let fb = |state: &str, q: &str| json!({ "id": "fb", "name": "Facebook", "state": state, "quality": q });

        // Not streaming: reports are kept but nothing is shown and nothing is news.
        report(&mut e, json!([yt("off", ""), fb("off", "")]));
        assert!(send(&mut e, json!({ "type": "state.get" }))["state"].get("streamDestinations").is_none());

        send(&mut e, json!({ "type": "stream.start" }));
        report(&mut e, json!([yt("live", "1080p"), fb("live", "1080p")]));
        let state = send(&mut e, json!({ "type": "state.get" }))["state"].clone();
        assert_eq!(state["streamDestinations"][1], fb("live", "1080p"));
        assert_eq!(alerts(&rx), Vec::<Value>::new(), "starting isn't news");

        report(&mut e, json!([yt("live", "720p"), fb("paused", "")]));
        let got = alerts(&rx);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!((got[0]["kind"].clone(), got[0]["quality"].clone()), (json!("stream.lowered"), json!("720p")));
        assert_eq!(
            got[1]["message"],
            "Facebook is paused: the internet upload can't carry it right now. It comes back by itself when the upload recovers."
        );
        report(&mut e, json!([yt("live", "720p"), fb("paused", "")]));
        assert!(alerts(&rx).is_empty(), "said once, not every second");

        // The video process restarts: its first report after the gap isn't compared.
        e.stream_report = e.stream_report.map(|(s, at)| (s, at - STREAM_REPORT_STALE));
        assert!(
            send(&mut e, json!({ "type": "state.get" }))["state"].get("streamDestinations").is_none(),
            "stale: not shown"
        );
        report(&mut e, json!([yt("connecting", "1080p"), fb("paused", "")]));
        report(&mut e, json!([yt("live", "1080p"), fb("live", "360p")]));
        let got = alerts(&rx);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0]["kind"], "stream.resumed");
    }

    #[test]
    fn an_older_video_process_without_destinations_still_works() {
        let (mut e, _) = engine("alerts-old", 10);
        send(&mut e, json!({ "type": "stream.start" }));
        let yt = json!([{ "id": "yt", "name": "YouTube", "state": "live", "quality": "1080p" }]);
        report(&mut e, yt);
        assert!(send(&mut e, json!({ "type": "state.get" }))["state"].get("streamDestinations").is_some());
        // Replaced by an older one that doesn't list them: the old list goes.
        let r = send(&mut e, json!({ "type": "stream.report", "status": "live" }));
        assert_eq!(r["ok"], true);
        let state = send(&mut e, json!({ "type": "state.get" }))["state"].clone();
        assert_eq!(state["stream"], "live");
        assert!(state.get("streamDestinations").is_none());
    }

    /// An engine that can finish work on other threads, as the real one can.
    fn shared_engine(name: &str, url: &str) -> Arc<Mutex<Engine>> {
        let (e, _) = engine(name, 10);
        let shared = Arc::new(Mutex::new(e));
        let mut e = shared.lock().unwrap();
        e.set_handle(Arc::downgrade(&shared));
        e.set_upload_test_url(url);
        drop(e);
        shared
    }

    fn wait_event(rx: &std::sync::mpsc::Receiver<Vec<u8>>, name: &str) -> Value {
        loop {
            let b = rx.recv_timeout(Duration::from_secs(20)).expect("an event in time");
            let v: Value = serde_json::from_slice(&b).unwrap();
            if v["event"] == name {
                return v;
            }
        }
    }

    #[test]
    fn preflight_tests_the_upload_and_everyone_hears_what_each_platform_will_get() {
        let shared = shared_engine("preflight", &crate::preflight::tests::fake_endpoint(20_000));
        std::fs::write(
            shared.lock().unwrap().store.dir().join("stream.json"),
            r#"{"version":1,"destinations":[{"id":"youtube","name":"YouTube","url":"rtmps://h/live2","key":"k"}]}"#,
        )
        .unwrap();
        let (tx, screen) = sync_channel(64);
        shared
            .lock()
            .unwrap()
            .handle_line_from(&envelope(json!({ "type": "state.subscribe" })), Some(&Outbox::new(tx, None)));
        // Asked by a connection that isn't subscribed: it gets the result too, once.
        let (tx, asker) = sync_channel(64);
        let ack = shared
            .lock()
            .unwrap()
            .handle_line_from(&envelope(json!({ "type": "stream.preflight" })), Some(&Outbox::new(tx, None)));
        let ack = serde_json::to_value(ack).unwrap();
        assert_eq!(ack["preflight"]["status"], "measuring");
        let heard = wait_event(&screen, "preflight");
        assert_eq!(heard["preflight"]["summary"], "YouTube 1080p", "{heard}");
        assert_eq!(heard["preflight"]["measured"], true);
        assert_eq!(wait_event(&asker, "preflight"), heard);
        assert!(asker.try_recv().is_err(), "once");
    }

    #[test]
    fn preflight_never_runs_while_streaming_and_the_stream_starting_stops_it() {
        let shared = shared_engine("preflight-stream", &crate::preflight::tests::fake_endpoint(1_000));
        let (tx, screen) = sync_channel(64);
        let mut e = shared.lock().unwrap();
        e.handle_line_from(&envelope(json!({ "type": "state.subscribe" })), Some(&Outbox::new(tx, None)));
        assert_eq!(send(&mut e, json!({ "type": "stream.preflight" }))["preflight"]["status"], "measuring");
        send(&mut e, json!({ "type": "stream.start" }));
        drop(e);
        let heard = wait_event(&screen, "preflight");
        assert_eq!(heard["preflight"]["status"], "failed");
        assert!(heard["preflight"]["message"].as_str().unwrap().contains("the stream started"));
        let mut e = shared.lock().unwrap();
        assert_eq!(send(&mut e, json!({ "type": "stream.preflight" }))["code"], "unavailable");
    }

    #[test]
    fn a_church_computer_screen_that_subscribes_late_still_sees_waiting_requests() {
        let (mut e, _) = engine("pair-late", 10);
        send(&mut e, json!({ "type": "pair.open" }));
        let mut phone = Phone::new();
        let req = phone
            .send(&mut e, json!({ "type": "pair.request", "publicKey": TestKey::new().public(), "name": "Phone" }));
        let (tx, rx) = sync_channel(64);
        e.handle_line_from(&envelope(json!({ "type": "state.subscribe" })), Some(&Outbox::new(tx, None)));
        let first: Value = serde_json::from_slice(&rx.try_recv().expect("the pairing status at once")).unwrap();
        assert_eq!(first["event"], "pairing");
        assert_eq!(first["pairing"]["requests"][0]["code"], req["request"]["code"]);

        // A device that subscribes hears nothing about pairing.
        let (tx, rx) = sync_channel(64);
        let mut session = Session::device();
        let key = TestKey::new();
        e.devices.add("D", &key.public(), 1).unwrap();
        let o = Outbox::new(tx, None);
        let hello =
            e.handle(&envelope(json!({ "type": "auth.hello", "publicKey": key.public() })), &mut session, Some(&o));
        let hello = serde_json::to_value(hello).unwrap();
        let msg = protocol::auth_message(e.daemon_id(), hello["challenge"].as_str().unwrap());
        e.handle(&envelope(json!({ "type": "auth.prove", "signature": key.sign(&msg) })), &mut session, Some(&o));
        e.handle(&envelope(json!({ "type": "state.subscribe" })), &mut session, Some(&o));
        assert!(rx.try_iter().all(|b| serde_json::from_slice::<Value>(&b).unwrap()["event"] != "pairing"));
    }

    #[test]
    fn a_paired_device_can_never_do_the_church_computers_or_the_video_processs_jobs() {
        let (mut e, _) = engine("pair-limits", 10);
        let key = TestKey::new();
        pair(&mut e, &key);
        let mut phone = Phone::new();
        phone.prove(&mut e, &key);
        for command in [
            json!({ "type": "pair.open" }),
            json!({ "type": "pair.close" }),
            json!({ "type": "pair.approve", "requestId": "x" }),
            json!({ "type": "pair.deny", "requestId": "x" }),
            json!({ "type": "device.list" }),
            json!({ "type": "device.remove", "deviceId": "x" }),
            json!({ "type": "stream.report", "status": "live" }),
            json!({ "type": "recording.report", "status": "recording" }),
            json!({ "type": "media.levels", "peakDb": [0], "rmsDb": [0] }),
        ] {
            assert_eq!(phone.send(&mut e, command.clone())["code"], "unauthorized", "{command}");
        }
    }

    #[test]
    fn only_the_right_key_for_this_daemon_on_a_fresh_challenge_gets_in() {
        let (mut e, _) = engine("pair-proof", 10);
        let (key, stranger) = (TestKey::new(), TestKey::new());
        pair(&mut e, &key);
        assert_eq!(Phone::new().prove(&mut e, &stranger)["code"], "unauthorized");

        // Signed for another church computer.
        let mut phone = Phone::new();
        let hello = phone.send(&mut e, json!({ "type": "auth.hello", "publicKey": key.public() }));
        let elsewhere = protocol::auth_message("another-church", hello["challenge"].as_str().unwrap());
        assert_eq!(
            phone.send(&mut e, json!({ "type": "auth.prove", "signature": key.sign(&elsewhere) }))["code"],
            "unauthorized"
        );

        // A signature works once, on its own connection.
        let mut a = Phone::new();
        let hello = a.send(&mut e, json!({ "type": "auth.hello", "publicKey": key.public() }));
        let sig = key.sign(&protocol::auth_message(e.daemon_id(), hello["challenge"].as_str().unwrap()));
        let mut b = Phone::new();
        b.send(&mut e, json!({ "type": "auth.hello", "publicKey": key.public() }));
        assert_eq!(b.send(&mut e, json!({ "type": "auth.prove", "signature": sig.clone() }))["code"], "unauthorized");
        assert_eq!(a.send(&mut e, json!({ "type": "auth.prove", "signature": sig.clone() }))["ok"], true);
        assert_eq!(
            Phone::new().send(&mut e, json!({ "type": "auth.prove", "signature": sig }))["code"],
            "unauthorized"
        );

        // A wrong proof uses up the challenge.
        let mut c = Phone::new();
        let hello = c.send(&mut e, json!({ "type": "auth.hello", "publicKey": key.public() }));
        let msg = protocol::auth_message(e.daemon_id(), hello["challenge"].as_str().unwrap());
        c.send(&mut e, json!({ "type": "auth.prove", "signature": stranger.sign(&msg) }));
        assert_eq!(
            c.send(&mut e, json!({ "type": "auth.prove", "signature": key.sign(&msg) }))["code"],
            "unauthorized"
        );
    }

    #[test]
    fn pairing_closes_by_itself_after_two_minutes_and_waiting_devices_hear_no() {
        let (mut e, _) = engine("pair-window", 10);
        let key = TestKey::new();
        send(&mut e, json!({ "type": "pair.open" }));
        let mut phone = Phone::new();
        let req = phone.send(&mut e, json!({ "type": "pair.request", "publicKey": key.public(), "name": "Phone" }));
        e.tick_at(Instant::now() + protocol::PAIRING_WINDOW);
        let result = phone.events().pop().unwrap();
        assert_eq!((result["event"].clone(), result["approved"].clone()), (json!("pair.result"), json!(false)));
        let late = send(&mut e, json!({ "type": "pair.approve", "requestId": req["request"]["requestId"] }));
        assert_eq!(late["code"], "bad_arguments");
        let again = phone.send(&mut e, json!({ "type": "pair.request", "publicKey": key.public(), "name": "Phone" }));
        assert_eq!(again["code"], "unauthorized");
        assert_eq!(send(&mut e, json!({ "type": "device.list" }))["devices"], json!([]));
    }

    #[test]
    fn a_denied_device_isnt_paired_and_five_requests_at_most_wait() {
        let (mut e, _) = engine("pair-deny", 10);
        send(&mut e, json!({ "type": "pair.open" }));
        let key = TestKey::new();
        let mut phone = Phone::new();
        let req = phone.send(&mut e, json!({ "type": "pair.request", "publicKey": key.public(), "name": "Phone" }));
        send(&mut e, json!({ "type": "pair.deny", "requestId": req["request"]["requestId"] }));
        assert_eq!(phone.events().pop().unwrap()["approved"], false);
        assert_eq!(Phone::new().prove(&mut e, &key)["code"], "unauthorized");

        let mut waiting = Vec::new();
        for i in 0..5 {
            let mut p = Phone::new();
            let r = p.send(
                &mut e,
                json!({ "type": "pair.request", "publicKey": TestKey::new().public(), "name": format!("P{i}") }),
            );
            assert_eq!(r["ok"], true);
            waiting.push(p);
        }
        let r = Phone::new()
            .send(&mut e, json!({ "type": "pair.request", "publicKey": TestKey::new().public(), "name": "P6" }));
        assert_eq!(r["code"], "unauthorized");
        // A device that leaves takes its request with it, making room.
        e.forget(&waiting.pop().unwrap().session);
        let r = Phone::new()
            .send(&mut e, json!({ "type": "pair.request", "publicKey": TestKey::new().public(), "name": "P6" }));
        assert_eq!(r["ok"], true);
    }

    #[test]
    fn paired_devices_survive_a_restart_and_a_removed_one_gets_nothing_more() {
        let (mut e, dir) = engine("pair-restart", 10);
        let key = TestKey::new();
        let device_id = pair(&mut e, &key);
        let daemon_id = e.daemon_id().to_string();
        let mut phone = Phone::new();
        phone.prove(&mut e, &key);
        drop(e);

        // A new engine on the same data folder: same daemon id, same devices.
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let mut e = Engine::new(store, snap, 10);
        assert_eq!(e.daemon_id(), daemon_id);
        let mut phone = Phone::new();
        assert_eq!(phone.prove(&mut e, &key)["ok"], true);

        assert_eq!(send(&mut e, json!({ "type": "device.remove", "deviceId": device_id }))["devices"], json!([]));
        assert_eq!(
            phone.send(&mut e, json!({ "type": "state.get" }))["code"],
            "unauthorized",
            "an open connection is cut off"
        );
        assert_eq!(Phone::new().prove(&mut e, &key)["code"], "unauthorized");
    }

    #[test]
    fn removing_a_device_closes_its_websocket_at_once() {
        let (tcp, ws_addr) = serve_both("pair-remove", &[]);
        let key = TestKey::new();
        let mut ws = ws_connect(ws_addr, None).unwrap();
        pair_over_the_wire(tcp, &mut ws, &key);
        let devices = tcp_ask(tcp, &envelope(json!({ "type": "device.list" })));
        let device_id = devices["devices"][0]["deviceId"].as_str().unwrap().to_string();
        tcp_ask(tcp, &envelope(json!({ "type": "device.remove", "deviceId": device_id })));
        let start = Instant::now();
        let end = loop {
            match ws.read() {
                Ok(_) => continue,
                Err(e) => break e,
            }
        };
        assert!(
            !matches!(end, tungstenite::Error::Io(ref e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "closed, not timed out: {end}"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn plain_websocket_is_only_for_this_computer() {
        for addr in ["127.0.0.1:47801", "[::1]:47801", "localhost:47801"] {
            assert!(is_loopback_only(addr), "{addr}");
        }
        for addr in ["0.0.0.0:47801", "[::]:47801", "192.168.1.20:47801", "10.0.0.1:1", "nonsense", ""] {
            assert!(!is_loopback_only(addr), "{addr}");
        }
    }

    #[test]
    fn the_local_channel_only_takes_connections_from_this_computer() {
        // Needs an address on this machine that isn't loopback; skipped without one.
        let Ok(probe) = std::net::UdpSocket::bind("0.0.0.0:0") else { return };
        if probe.connect("192.0.2.1:9").is_err() {
            eprintln!("skipped: no route off this machine");
            return;
        }
        let Ok(local) = probe.local_addr() else { return };
        if local.ip().is_unspecified() || local.ip().is_loopback() {
            eprintln!("skipped: this machine has no address but loopback");
            return;
        }
        eprintln!("connecting from {}", local.ip());
        let (e, _) = engine("local-only", 10);
        let listener = bind("0.0.0.0:0", Duration::from_secs(1)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || serve(listener, Arc::new(Mutex::new(e))));
        let mut s = TcpStream::connect((local.ip(), port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(format!("{}\n", envelope(json!({ "type": "state.get" }))).as_bytes()).unwrap();
        let mut reply = String::new();
        let n = BufReader::new(s).read_line(&mut reply).unwrap_or(0);
        assert_eq!(n, 0, "no answer to a connection from {}: {reply}", local.ip());
    }
}
