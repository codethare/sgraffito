//! The AI mind-map panel's non-UI half: which endpoint a request goes to, the sandboxed streaming
//! request that asks the model for a Mermaid `mindmap`, the slice of Mermaid this build understands,
//! and the radial layout the answer is drawn and committed as.
//!
//! Nothing here is persisted. A chat transcript, the input and the preview live in memory for one
//! process; only the diagram the preview is committed as reaches the document, and it is ordinary
//! shapes and texts there — selectable, editable and undoable like anything drawn by hand.
//!
//! The request runs in `bwrap` because it leaves the machine. `curl` is the transport rather than a
//! Rust HTTPS client: the dependency graph has no TLS stack, and adding one for a single streaming
//! call is not worth the port. The key never enters an argv: it is passed in the sandbox's own
//! environment, and the script reads it from there.

use std::ffi::OsString;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use calloop::ping::Ping;
use serde_json::{Value, json};

use crate::canvas::{
    AiMessageView, AiView, Attachment, Board, Shape, ShapeKind, TextBuffer, TextItem,
};
use crate::config::{Config, DEFAULT_AI_MODEL};

/// What the model is asked for. One fenced `mindmap`, nothing after it: the parser reads the last
/// fenced block, so a correction after an earlier attempt replaces it rather than adding to it.
pub const SYSTEM_PROMPT: &str = "\
You create Mermaid mind maps for the sgraffito drawing board.
Answer in the user's language. Keep prose to at most two short sentences.
Then output exactly one fenced code block with language `mermaid` and a `mindmap` diagram.
The block starts with `mindmap`, then one comment line naming the arrangement:
  %% sgraffito layout: radial | tree | fishbone | compare
Use two spaces per indentation level. One root and at most 40 nodes. Do not add any text after the block.

Choose the arrangement by what the map is for:
- radial: brainstorming a topic, no fixed order (发散).
- tree: decomposing something into parts, two levels or more below the root (分解).
- fishbone: the causes of one effect; the root is the effect, each first-level node is a cause group (因果).
- compare: exactly two first-level nodes, the two things compared; a child whose name appears under both is
  an attribute they share (对比).

Rules for the content:
1. MECE: siblings at one level must not overlap and must cover their parent. Never mix dimensions at one
   level — time next to place is wrong.
2. One granularity per level: all nouns, or all steps, not a mix.
3. Each branch holds 5±2 children. A longer list gets grouped under a new parent.
4. Nodes are keywords, not sentences: one word or a short phrase. Detail goes into a parent, not a clause.
5. At most 3 to 4 levels; deeper detail belongs in the parent's text.

Order: first level clockwise from the right, most important first; a tree is read top to bottom.";
/// The longest prompt that is sent, in bytes. A prompt this long is a paste accident, not a request.
pub const MAX_PROMPT_BYTES: usize = 10_000;
/// The longest answer that is kept, in bytes. Past this the request is aborted and what arrived stays
/// on screen, so a runaway generation cannot grow without bound.
pub const MAX_ASSISTANT_BYTES: usize = 64 * 1024;
/// How many earlier messages are sent back, newest first, so a follow-up can refer to the previous
/// answer without sending an unbounded transcript.
pub const MAX_HISTORY_MESSAGES: usize = 24;
/// The most nodes a parsed mind map may hold. Nodes beyond this are refused rather than drawn over
/// half the board.
pub const MAX_MINDMAP_NODES: usize = 80;
/// How often a streaming answer is re-parsed for the live preview. The model arrives in tokens; a
/// preview per token would re-lay out the whole diagram for no visible gain.
pub const PREVIEW_INTERVAL: Duration = Duration::from_millis(120);
/// The wait between frames while an answer streams.
pub const STREAM_FRAME: Duration = Duration::from_millis(33);

/// How far one wheel notch moves the transcript, in logical pixels, before a burst multiplies it.
pub const SCROLL_PIXELS: f32 = 48.0;
/// Two scroll steps this close together are one burst; a burst lives this long after its last step.
pub const BURST_GAP: Duration = Duration::from_millis(150);
pub const BURST_WINDOW: Duration = Duration::from_millis(400);
/// How long the strip keeps the multiplier after the last notch. The acceleration itself ends with the
/// window above; the number is information, and 400 ms is not long enough to read it while the wheel is
/// still spinning.
pub const BURST_HOLD: Duration = Duration::from_millis(900);
/// What each further step of a burst adds, and the most it can become. A wheel is a notch at a time
/// and accelerates; a touchpad's events inside one gesture are one step, so it is repeated flicks that
/// speed up — the rule the reference implementation uses, which is what makes the number on the strip
/// mean the same thing in both.
pub const BURST_GAIN: f32 = 0.5;
pub const BURST_MAX: f32 = 4.0;
/// How fast the drawn offset catches up with what the wheel asked for, in seconds. A time constant of
/// 45 ms is about the rail's own 140 ms transition, so scrolling and the interface move at one speed.
pub const SCROLL_TAU: f32 = 0.045;

const GO_BASE: &str = "https://opencode.ai/inference/go";
const ZEN_BASE: &str = "https://opencode.ai/inference";

/// Everything one request needs, after the environment and the file have been folded together.
/// `Debug` redacts the key: this value is logged and logged values end up in bug reports.
#[derive(Clone, PartialEq)]
pub struct AiSettings {
    pub endpoint: String,
    pub model: String,
    pub key: String,
    pub timeout: Duration,
}

impl fmt::Debug for AiSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AiSettings")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("key", &if self.key.is_empty() { "" } else { "<set>" })
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Resolve with the process environment. Read on every request, not once at startup, so exporting a
/// key in a running session is enough.
pub fn resolve(config: &Config) -> Result<AiSettings, String> {
    resolve_with(config, |name| std::env::var(name).ok())
}

/// The resolution rules, with the environment injected so they can be tested without touching it.
///
/// A specific Go or Zen key picks its own base. A generic `OPENCODE_API_KEY` is taken to be a Go key,
/// which is the account this build is aimed at; a Zen-only user with that one variable names its base
/// in `[ai].base_url`. A configured `base_url` with no key at all is allowed: a local gateway or a
/// proxy that authenticates for the user needs no bearer.
pub fn resolve_with(
    config: &Config,
    get: impl Fn(&str) -> Option<String>,
) -> Result<AiSettings, String> {
    let mut key = String::new();
    let mut base = String::new();
    for (name, candidate) in [
        ("OPENCODE_GO_API_KEY", GO_BASE),
        ("OPENCODE_ZEN_API_KEY", ZEN_BASE),
        ("OPENCODE_API_KEY", GO_BASE),
    ] {
        let value = get(name).unwrap_or_default();
        let value = value.trim();
        if !value.is_empty() {
            key = value.to_string();
            base = candidate.to_string();
            break;
        }
    }
    let configured_key = config.ai.api_key.trim();
    if key.is_empty() && !configured_key.is_empty() {
        key = configured_key.to_string();
        base = GO_BASE.to_string();
    }
    // Trimmed here too, not only in `load_from`: a value built in code is held to the same rule, so a
    // base of one space is empty rather than a path with no host.
    let configured_base = config.ai.base_url.trim();
    if !configured_base.is_empty() {
        base = configured_base.to_string();
    }
    if key.is_empty() && base.is_empty() {
        return Err(
            "set OPENCODE_GO_API_KEY, OPENCODE_ZEN_API_KEY, OPENCODE_API_KEY, or [ai].base_url"
                .into(),
        );
    }
    let model = if config.ai.model.is_empty() {
        DEFAULT_AI_MODEL.to_string()
    } else {
        config.ai.model.clone()
    };
    let timeout = config
        .ai
        .timeout
        .clamp(Duration::from_secs(5), Duration::from_secs(600));
    Ok(AiSettings {
        endpoint: endpoint_for(&base),
        model,
        key,
        timeout,
    })
}

/// The URL a request is posted to, from the base the file or the key chose.
///
/// A base may be a host, a host plus `/openai`, or a host plus `/v1`, so only the missing part of the
/// path is appended. Appending the whole path to a base that already ends in `/v1` builds
/// `.../v1/openai/v1/chat/completions`, which every gateway answers with 404 — the documented
/// OpenCode bases (`/inference/go`, `/inference`) take the full suffix, while
/// `https://opencode.ai/zen/go/v1` takes only `/chat/completions`.
fn endpoint_for(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else if base.ends_with("/openai") {
        format!("{base}/v1/chat/completions")
    } else {
        format!("{base}/openai/v1/chat/completions")
    }
}

/// Who said one line of the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One line of the transcript, as it is drawn and as it is sent back as history.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub text: String,
    /// The one answer still arriving. Drawn with a cursor at its end.
    pub streaming: bool,
}

/// What the reader thread reports to the loop.
#[derive(Debug, Clone, PartialEq)]
pub enum AiEvent {
    Delta(String),
    Done(u16),
    Error(String),
}

/// The child of a request in flight. Dropping it kills and reaps it: a stream nobody reads to the end
/// must not outlive the panel.
struct Generation {
    child: Child,
}

impl Drop for Generation {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The AI panel: its transcript, the request in flight, and the preview the stream is producing.
///
/// Nothing here is serialized and nothing here reaches the document until [`Chat::view`]'s preview is
/// committed by the app. A restart therefore begins with an empty panel, which is deliberate: a
/// transcript is a conversation, not content.
/// One scroll burst: how many steps it is on, and when its last one arrived.
///
/// A wheel spun quickly accelerates — the second notch moves half again as far as the first, up to
/// four times — and stops doing so 150 ms after the last notch. A touchpad's events inside one gesture
/// are a single step, because they are already continuous; it is repeated flicks, separated by the
/// gesture's own break, that accelerate.
#[derive(Debug, Clone, Copy, Default)]
pub struct Burst {
    steps: u32,
    last: Option<Instant>,
}

impl Burst {
    /// Take one scroll event and answer the factor it moves with.
    pub fn take(&mut self, now: Instant, wheel: bool) -> f32 {
        let since = self
            .last
            .map_or(Duration::MAX, |at| now.saturating_duration_since(at));
        self.last = Some(now);
        let inside = since < BURST_GAP;
        self.steps = match (wheel, inside) {
            // A notch after a notch is the next step; a notch after a pause starts over.
            (true, true) => self.steps.saturating_add(1),
            (true, false) => 1,
            // Inside one gesture: the burst has not moved on.
            (false, true) => self.steps.max(1),
            // A fresh gesture inside the burst's window is the next step; later, it is a new burst.
            (false, false) if since < BURST_WINDOW => self.steps.max(1).saturating_add(1),
            (false, false) => 1,
        };
        self.factor(now)
    }

    /// What the burst multiplies a step by right now, or one once its window has passed.
    pub fn factor(&self, now: Instant) -> f32 {
        match self.last {
            Some(at) if now.saturating_duration_since(at) < BURST_WINDOW => {
                (1.0 + BURST_GAIN * self.steps.saturating_sub(1) as f32).min(BURST_MAX)
            }
            _ => 1.0,
        }
    }

    /// What the strip says: the factor while the hold lasts, which is longer than the acceleration so
    /// the number can be read.
    pub fn label(&self, now: Instant) -> f32 {
        match self.last {
            Some(at) if now.saturating_duration_since(at) < BURST_HOLD => {
                (1.0 + BURST_GAIN * self.steps.saturating_sub(1) as f32).min(BURST_MAX)
            }
            _ => 1.0,
        }
    }

    /// How long the strip keeps saying it, so the loop can wake exactly when it has to go.
    pub fn hold_left(&self, now: Instant) -> Option<Duration> {
        let at = self.last?;
        let since = now.saturating_duration_since(at);
        (self.label(now) > 1.0).then(|| BURST_HOLD.saturating_sub(since))
    }
}

/// The multiplier as the strip carries it: one decimal only when it is not whole, so the halves of the
/// ramp read as `1.5x` and the whole ones as `4x`.
pub fn burst_label(factor: f32) -> String {
    if (factor - factor.round()).abs() < 0.05 {
        format!("{}x", factor.round() as i32)
    } else {
        format!("{factor:.1}x")
    }
}

/// One board's conversation: its transcript, its draft, where its diagram goes, and the diagram it
/// placed last time.
///
/// Every board has its own, so a prompt written while one board is on screen can never continue
/// another board's conversation, reuse its context, or replace its diagram.
#[derive(Debug, Clone)]
pub struct Session {
    pub messages: Vec<Message>,
    pub input: TextBuffer,
    pub preedit: String,
    pub error: Option<String>,
    /// How far the log is scrolled, in logical pixels from its top, as it is drawn.
    pub scroll: f32,
    /// Where the wheel has asked the log to be. The drawn offset eases toward it, so a notch glides
    /// instead of jumping.
    pub scroll_target: f32,
    /// Whether the log follows the end of the stream. A user who scrolls up stops following.
    pub follow: bool,
    /// The burst the wheel is in, and the factor that was last drawn, so the strip can be repainted
    /// once when the number expires.
    pub burst: Burst,
    pub burst_drawn: f32,
    /// The left edge of the room the panel leaves, in canvas coordinates, and how wide it is. A ring
    /// grows from its middle; a tree starts at its left edge.
    pub anchor: [f32; 2],
    pub room: f32,
    /// The ink its diagram is drawn in.
    pub color: String,
    /// The diagram as it stands, drawn live only while its own board is the one on screen.
    pub preview: Option<Board>,
    /// The ids of the shapes this conversation placed last time, so a follow-up replaces its own
    /// diagram and nobody else's.
    pub placed_shapes: Vec<u64>,
    /// When the preview was last rebuilt.
    pub last_preview: Instant,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            input: TextBuffer::new(""),
            preedit: String::new(),
            error: None,
            scroll: 0.0,
            scroll_target: 0.0,
            follow: true,
            burst: Burst::default(),
            burst_drawn: 1.0,
            anchor: [0.0, 0.0],
            room: 0.0,
            color: String::new(),
            preview: None,
            placed_shapes: Vec::new(),
            last_preview: Instant::now(),
        }
    }

    fn last_assistant(&self) -> Option<&Message> {
        self.messages.last().filter(|m| m.role == Role::Assistant)
    }

    pub fn push(&mut self, role: Role, text: String, streaming: bool) {
        self.messages.push(Message {
            role,
            text,
            streaming,
        });
    }

    /// Stop the cursor at the end of an answer that will not grow any further.
    pub fn stop_streaming(&mut self) {
        if let Some(last) = self.messages.last_mut() {
            last.streaming = false;
        }
    }

    /// Move the view for one scroll event. `raw` is already in logical pixels; the burst decides what
    /// it is multiplied by, and the drawn offset catches up over the next frames.
    pub fn scroll_by(&mut self, raw: f32, wheel: bool, now: Instant, max: f32) {
        if raw == 0.0 {
            return;
        }
        let factor = self.burst.take(now, wheel);
        self.scroll_target = (self.scroll_target + raw * factor).clamp(0.0, max);
        self.follow = self.scroll_target >= max;
    }

    /// Ease the drawn offset toward the wheel's. Returns whether it moved, so a frame is only spent on
    /// a scroll that is actually going somewhere.
    pub fn ease_scroll(&mut self, dt: f32) -> bool {
        let delta = self.scroll_target - self.scroll;
        if delta.abs() < 0.05 {
            let landed = self.scroll != self.scroll_target;
            self.scroll = self.scroll_target;
            return landed;
        }
        // Frame-rate independent: the fraction covered in `dt` does not depend on how many frames the
        // loop managed, so a dropped frame slows nothing down.
        let covered = 1.0 - (-dt / SCROLL_TAU).exp();
        self.scroll += delta * covered.clamp(0.0, 1.0);
        true
    }

    /// Whether the drawn offset is still on its way somewhere.
    pub fn scrolling(&self) -> bool {
        self.scroll != self.scroll_target
    }

    /// Keep both the wheel's target and the drawn offset inside the transcript, and follow the end
    /// when the reader has not scrolled away from it.
    pub fn clamp_scroll(&mut self, max: f32) {
        self.scroll_target = if self.follow {
            max
        } else {
            self.scroll_target.clamp(0.0, max)
        };
        self.scroll = self.scroll.clamp(0.0, max);
    }
}

/// What one frame of the panel's motion changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Moved {
    /// The transcript's offset moved: nothing outside the log's window changed.
    pub scroll: bool,
    /// Something the whole card shows changed, such as the strip's multiplier.
    pub view: bool,
}

/// The AI panel: one window, showing the conversation of the board on screen.
///
/// The window is one and its conversations are many. The transcript, the draft, the error, the scroll
/// and the diagram belong to a board, so switching boards switches which conversation is shown and
/// nothing else: no board can read another's context or replace another's diagram.
pub struct Chat {
    pub open: bool,
    /// The output the panel is drawn on. One panel, on one screen.
    pub output: Option<u32>,
    /// Whether the input line owns the keyboard.
    pub focus: bool,
    /// The board whose conversation is shown, and the one a prompt is sent against.
    board: String,
    sessions: std::collections::BTreeMap<String, Session>,
    /// The session header the Go endpoint wants. One id per process, so a follow-up continues it.
    pub api_session: String,
    /// A press that went down inside the panel owns the pointer until it is released.
    pub pointer_captured: bool,
    /// When the panel last opened: its card slides into place over the rail's own transition, and this
    /// is the clock that drives it. `None` until it has opened once.
    pub open_since: Option<Instant>,
    /// The board whose answer is still arriving. A delta lands in that board's conversation, never in
    /// whichever board happens to be on screen when it arrives.
    streaming_board: Option<String>,
    generation: Option<Generation>,
    rx: Option<mpsc::Receiver<AiEvent>>,
}

impl Default for Chat {
    fn default() -> Self {
        Self::new()
    }
}

impl Chat {
    pub fn new() -> Self {
        let mut sessions = std::collections::BTreeMap::new();
        sessions.insert(String::new(), Session::new());
        Self {
            open: false,
            output: None,
            focus: false,
            board: String::new(),
            sessions,
            api_session: session_id(),
            pointer_captured: false,
            open_since: None,
            streaming_board: None,
            generation: None,
            rx: None,
        }
    }

    /// The board whose conversation is shown.
    pub fn board(&self) -> &str {
        &self.board
    }

    /// The conversation shown. A board that has never been asked anything has an empty one, created on
    /// first sight so the panel can always draw something.
    pub fn session(&self) -> &Session {
        self.sessions
            .get(&self.board)
            .expect("a shown board always has a conversation")
    }

    pub fn session_mut(&mut self) -> &mut Session {
        self.sessions.entry(self.board.clone()).or_default()
    }

    /// Show `board`'s own conversation. Returns whether the shown board changed, so the caller can
    /// stop a request that belonged to the board being left.
    pub fn enter(&mut self, board: &str) -> bool {
        if self.board == board {
            self.sessions.entry(self.board.clone()).or_default();
            return false;
        }
        // A preedit is a composition in flight, not content: it belongs to the board it was begun on.
        if let Some(previous) = self.sessions.get_mut(&self.board) {
            previous.preedit.clear();
        }
        self.board = board.to_string();
        self.sessions.entry(self.board.clone()).or_default();
        true
    }

    /// Forget a board's conversation: the board itself is gone. A panel still showing that board keeps
    /// an empty conversation rather than a hole, so the window can always be drawn.
    pub fn forget(&mut self, board: &str) {
        self.sessions.remove(board);
        if self.board == board {
            self.sessions.insert(self.board.clone(), Session::new());
        }
    }

    /// Clear the shown conversation's live preview: the answer it belonged to is not coming.
    pub fn clear_preview(&mut self) {
        self.session_mut().preview = None;
    }

    /// Record a failure against `board`'s conversation, whether or not that board is on screen.
    pub fn fail(&mut self, board: &str, reason: &str) {
        let session = self.sessions.entry(board.to_string()).or_default();
        session.preview = None;
        session.error = Some(reason.to_string());
    }

    /// The board an answer is arriving for, if one is.
    pub fn streaming_board(&self) -> Option<&str> {
        self.streaming_board.as_deref()
    }

    /// Whether a request is still arriving.
    pub fn streaming(&self) -> bool {
        self.generation.is_some()
    }

    /// The transcript and input as the renderer draws them. A copy: the window's state is the app's.
    pub fn view(&self) -> AiView {
        let session = self.session();
        AiView {
            messages: session
                .messages
                .iter()
                .map(|message| AiMessageView {
                    user: message.role == Role::User,
                    text: message.text.clone(),
                    streaming: message.streaming,
                })
                .collect(),
            input: session.input.clone(),
            preedit: session.preedit.clone(),
            streaming: self
                .streaming_board
                .as_deref()
                .is_some_and(|board| board == self.board),
            error: session.error.clone(),
            scroll: session.scroll,
            burst: session.burst_drawn,
            grow: 1.0,
        }
    }

    /// Move the shown transcript for one scroll event, in logical pixels.
    pub fn scroll_by(&mut self, raw: f32, wheel: bool, now: Instant, max: f32) {
        self.session_mut().scroll_by(raw, wheel, now, max);
    }

    /// Whether the shown transcript is still catching up with the wheel.
    pub fn scrolling(&self) -> bool {
        self.session().scrolling()
    }

    /// What the strip carries right now, and how long until it has to be cleared.
    pub fn burst_label(&self, now: Instant) -> f32 {
        self.session().burst.label(now)
    }

    pub fn burst_hold_left(&self, now: Instant) -> Option<Duration> {
        self.session().burst.hold_left(now)
    }

    /// One frame of the panel's own motion: the transcript easing toward the wheel's offset, and the
    /// burst's number leaving the strip when its window passes.
    pub fn step(&mut self, now: Instant, dt: f32) -> Moved {
        let session = self.session_mut();
        let scrolled = session.ease_scroll(dt);
        let factor = session.burst.label(now);
        let relabelled = session.burst_drawn != factor;
        if relabelled {
            session.burst_drawn = factor;
        }
        Moved {
            scroll: scrolled,
            view: relabelled,
        }
    }

    /// Send the prompt. The caller has already appended the user's message and an empty assistant one,
    /// so the body is built from the transcript as it stands.
    pub fn start(&mut self, settings: AiSettings, ping: Ping) -> Result<(), String> {
        if self.streaming() {
            return Err("a request is already running".into());
        }
        let session = self.session();
        let Some(prompt) = session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .map(|message| message.text.clone())
        else {
            return Err("there is no prompt to send".into());
        };
        let body = request_body(&settings, &session.messages, &prompt).to_string();
        crate::log::write(&format!(
            "ai: board {}, {} model {}, key {}",
            self.board,
            settings.endpoint,
            settings.model,
            if settings.key.is_empty() {
                "absent"
            } else {
                "set"
            }
        ));
        let config = curl_config(&settings, &self.api_session, &body);
        let mut child = Command::new("bwrap")
            .args(sandbox_args())
            .arg("--")
            .arg(curl_path())
            .args(CURL_ARGS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start bwrap: {e}"))?;
        if let Some(mut stdin) = child.stdin.take()
            && let Err(e) = stdin.write_all(config.as_bytes())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not send the request: {e}"));
        }
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the sandbox has no output to read".into());
        };
        let (tx, rx) = mpsc::channel();
        let key_present = !settings.key.is_empty();
        thread::spawn(move || read_stream(stdout, tx, ping, key_present));
        self.generation = Some(Generation { child });
        self.rx = Some(rx);
        self.streaming_board = Some(self.board.clone());
        Ok(())
    }

    /// Drain what the reader has reported. `true` when anything changed on screen.
    pub fn poll(&mut self) -> bool {
        let mut events = Vec::new();
        let Some(rx) = self.rx.as_ref() else {
            return false;
        };
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        if events.is_empty() {
            return false;
        }
        // The answer belongs to the board it was asked on, even if another board is on screen now.
        let board = self.streaming_board.clone().unwrap_or_default();
        let session = self.sessions.entry(board).or_default();
        let mut changed = false;
        let mut finished = false;
        let mut oversized = false;
        for event in events {
            match event {
                AiEvent::Delta(delta) => {
                    let Some(last) = session.last_assistant().map(|m| m.text.len()) else {
                        continue;
                    };
                    if last + delta.len() > MAX_ASSISTANT_BYTES {
                        oversized = true;
                        break;
                    }
                    if let Some(message) = session.messages.last_mut() {
                        message.text.push_str(&delta);
                    }
                    changed = true;
                }
                AiEvent::Done(_status) => finished = true,
                AiEvent::Error(message) => {
                    session.error = Some(message);
                    changed = true;
                    finished = true;
                }
            }
        }
        if oversized {
            session.error = Some("AI response too large".into());
            changed = true;
            finished = true;
        }
        if finished {
            // Dropping the receiver stops the reader as soon as its next send fails, and dropping the
            // generation kills and reaps the child.
            self.rx = None;
            self.generation = None;
            self.streaming_board = None;
            session.stop_streaming();
        }
        changed
    }

    /// Stop the request in flight, keeping what has already arrived. Closing the panel is one caller;
    /// a failed or stopped request is another.
    pub fn abort(&mut self) {
        self.rx = None;
        self.generation = None;
        let board = self
            .streaming_board
            .take()
            .unwrap_or_else(|| self.board.clone());
        if let Some(session) = self.sessions.get_mut(&board) {
            for message in &mut session.messages {
                message.streaming = false;
            }
        }
    }
}

/// One session id per process, which is what the Go endpoint groups a conversation by.
fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("ses_{nanos:016x}{:08x}", std::process::id())
}

/// The OpenAI-compatible request body: one system turn, the history that has landed, and the prompt.
fn request_body(settings: &AiSettings, messages: &[Message], prompt: &str) -> Value {
    let landed: Vec<&Message> = messages
        .iter()
        .take(messages.len().saturating_sub(2))
        .filter(|message| !message.streaming && !message.text.trim().is_empty())
        .collect();
    let history: Vec<Value> = landed[landed.len().saturating_sub(MAX_HISTORY_MESSAGES)..]
        .iter()
        .map(|message| {
            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            json!({ "role": role, "content": message.text })
        })
        .collect();
    let mut turns = Vec::with_capacity(history.len() + 2);
    turns.push(json!({ "role": "system", "content": SYSTEM_PROMPT }));
    turns.extend(history);
    turns.push(json!({ "role": "user", "content": prompt }));
    json!({
        "model": settings.model,
        "messages": turns,
        "stream": true,
    })
}

/// The arguments `curl` is given: unbuffered, and the whole configuration — URL, credential, body —
/// read from stdin, so nothing about the request is in the process table.
const CURL_ARGS: [&str; 3] = ["-N", "-K", "-"];

/// The sandbox's own arguments, without `curl`'s.
///
/// The environment inside is empty. Everything the request needs — endpoint, credential, session,
/// timeout and body — travels in curl's configuration on stdin, so it is in no argv and in no
/// environment of any process on the machine.
fn sandbox_args() -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--clearenv",
        "--unshare-all",
        "--share-net",
        "--die-with-parent",
        // No controlling terminal for the sandboxed process to inject keystrokes into.
        "--new-session",
        "--ro-bind",
        "/usr",
        "/usr",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    // `/bin`, `/lib` and `/lib64` are symlinks into `/usr` on most current distributions and real
    // directories on the rest. Binding the source of a symlink would put a directory where the
    // loader expects the link, so the link is reproduced as a link and a real directory is bound.
    for path in ["/bin", "/sbin", "/lib", "/lib64"] {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if let Ok(target) = std::fs::read_link(path) {
                    args.push("--symlink".into());
                    args.push(target.into());
                    args.push(path.into());
                }
            }
            Ok(_) => {
                args.push("--ro-bind".into());
                args.push(path.into());
                args.push(path.into());
            }
            Err(_) => {}
        }
    }
    // Only what this machine actually has: a distribution that keeps its CA bundle somewhere else is
    // not an error, it is one fewer bind.
    for path in [
        "/etc/ssl",
        "/etc/ca-certificates",
        "/etc/pki",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
    ] {
        if Path::new(path).exists() {
            args.push("--ro-bind".into());
            args.push(path.into());
            args.push(path.into());
        }
    }
    args.push("--proc".into());
    args.push("/proc".into());
    args.push("--dev".into());
    args.push("/dev".into());
    args
}

/// `curl` by absolute path, and from a directory the sandbox binds: with an empty environment there
/// is no `PATH` inside, so the path is resolved here.
fn curl_path() -> PathBuf {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("curl");
            if candidate.starts_with("/usr") && candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("/usr/bin/curl")
}

/// Escape for a curl configuration value: a quoted value ends at a quote and continues on the same
/// line, so the backslash, the quote and the whitespace that would end the line are escaped — the
/// backslash first, because every later pass introduces one.
fn escape_curl_config(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// The proxy lines for a curl configuration, from what the caller found in the environment. One value
/// per request: an https URL prefers `HTTPS_PROXY`, then `ALL_PROXY`, then `HTTP_PROXY`, which is
/// curl's own order.
fn proxy_config(
    https_proxy: Option<&str>,
    all_proxy: Option<&str>,
    http_proxy: Option<&str>,
    no_proxy: Option<&str>,
) -> String {
    let proxy = [https_proxy, all_proxy, http_proxy]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty());
    let mut lines = String::new();
    if let Some(proxy) = proxy {
        lines.push_str(&format!("proxy = \"{}\"\n", escape_curl_config(proxy)));
    }
    if let Some(no_proxy) = no_proxy.map(str::trim).filter(|value| !value.is_empty()) {
        lines.push_str(&format!("noproxy = \"{}\"\n", escape_curl_config(no_proxy)));
    }
    lines
}

/// The whole request as curl's own configuration, which the child reads on stdin: where to go, how to
/// authenticate, how long to wait, and the body. `-K -` is what keeps every one of them out of the
/// process table, and curling the body in the same file is why the request needs no second stream.
fn curl_config(settings: &AiSettings, session: &str, body: &str) -> String {
    let authorization = if settings.key.is_empty() {
        String::new()
    } else {
        format!(
            "header = \"Authorization: Bearer {}\"\n",
            escape_curl_config(&settings.key)
        )
    };
    let proxy = proxy_config(
        std::env::var("HTTPS_PROXY").ok().as_deref(),
        std::env::var("ALL_PROXY").ok().as_deref(),
        std::env::var("HTTP_PROXY").ok().as_deref(),
        std::env::var("NO_PROXY").ok().as_deref(),
    );
    format!(
        "url = \"{url}\"\n\
         request = \"POST\"\n\
         {authorization}\
         header = \"Content-Type: application/json\"\n\
         header = \"Accept: text/event-stream\"\n\
         header = \"x-opencode-session: {session}\"\n\
         {proxy}\
         no-buffer\n\
         connect-timeout = 15\n\
         max-time = {timeout}\n\
         silent\n\
         show-error\n\
         fail-with-body\n\
         data-binary = \"{body}\"\n\
         write-out = \"\\n__SGRAFFITO_HTTP__%{{response_code}}\\n\"\n",
        url = escape_curl_config(&settings.endpoint),
        session = escape_curl_config(session),
        timeout = settings.timeout.as_secs().max(1),
        body = escape_curl_config(body),
    )
}

/// Read the stream on its own thread: `data:` lines carry deltas, the write-out line carries the HTTP
/// status, and the end of the stream decides whether the request worked.
fn read_stream(
    stdout: impl std::io::Read,
    tx: mpsc::Sender<AiEvent>,
    ping: Ping,
    key_present: bool,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut status: Option<u16> = None;
    let mut body = String::new();
    let mut content = false;
    let mut reported = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = trimmed.strip_prefix("data:") {
            let data = rest.trim();
            if data == "[DONE]" {
                break;
            }
            if data.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if let Some(message) = value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
            {
                // The endpoint's own sentence replaces the raw body, and reading goes on: the HTTP
                // status arrives in the write-out after the body, and it is what decides whether this
                // is a credentials problem the user can fix.
                body = message.to_string();
                reported = true;
                continue;
            }
            if let Some(delta) = value
                .get("choices")
                .and_then(|choices| choices.get(0))
                .and_then(|choice| choice.get("delta"))
                .and_then(|delta| delta.get("content"))
                .and_then(Value::as_str)
            {
                content = true;
                report(&tx, &ping, AiEvent::Delta(delta.to_string()));
            }
            continue;
        }
        if let Some(code) = trimmed.strip_prefix("__SGRAFFITO_HTTP__") {
            status = code.trim().parse().ok();
            continue;
        }
        // `--fail-with-body` prints the error body and no events, so anything that is not an event
        // before the first delta is what the endpoint said instead of an answer.
        if !trimmed.trim().is_empty() && !content {
            body.push_str(trimmed);
            body.push('\n');
        }
    }
    let code = status.unwrap_or(200);
    if code >= 400 || reported {
        report(
            &tx,
            &ping,
            AiEvent::Error(status_message(code, &body, key_present)),
        );
    } else if !content {
        report(
            &tx,
            &ping,
            AiEvent::Error("the AI request ended without a response".into()),
        );
    } else {
        report(&tx, &ping, AiEvent::Done(code));
    }
}

/// How a failed request reads in the panel.
///
/// A gateway answering 401 or 403 with no key in the request is the one failure a user fixes in the
/// configuration, so the message names the ways in. The key is never echoed; nothing here is secret.
pub fn status_message(status: u16, body: &str, key_present: bool) -> String {
    let body = body.trim();
    let mut message = if status >= 400 {
        if body.is_empty() {
            format!("HTTP {status}")
        } else {
            format!("HTTP {status}: {body}")
        }
    } else if body.is_empty() {
        "the AI request failed".to_string()
    } else {
        body.to_string()
    };
    if !key_present && matches!(status, 401 | 403) {
        message.push_str(
            " (no API key was sent: set OPENCODE_GO_API_KEY, OPENCODE_ZEN_API_KEY, \
             OPENCODE_API_KEY, or [ai].api_key)",
        );
    }
    message
}

fn report(tx: &mpsc::Sender<AiEvent>, ping: &Ping, event: AiEvent) {
    if tx.send(event).is_ok() {
        ping.ping();
    }
}

/// How the layout asks for a text measurement: the text, its size, and the width to wrap at. The
/// renderer supplies it, so a box fits the glyphs the frame will draw rather than an estimate.
pub type TextMeasure<'a> = dyn FnMut(&str, f32, Option<f32>) -> (f32, f32) + 'a;

/// One node of a parsed mind map, as the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MindNode {
    pub text: String,
    pub children: Vec<MindNode>,
}

/// The last Mermaid block an answer contains. A model that corrects itself in the same message leaves
/// two blocks, and the correction is the one to draw.
pub fn extract_mermaid(text: &str) -> Option<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        match &mut current {
            Some(lines) => {
                if line.trim() == "```" {
                    blocks.push(lines.join("\n"));
                    current = None;
                } else {
                    lines.push(line);
                }
            }
            None => {
                if let Some(info) = line.trim().strip_prefix("```")
                    && info.trim_start().starts_with("mermaid")
                {
                    current = Some(Vec::new());
                }
            }
        }
    }
    // An unterminated block is what a streaming answer looks like for most of its life.
    if let Some(lines) = current {
        blocks.push(lines.join("\n"));
    }
    if let Some(last) = blocks.pop() {
        return Some(last);
    }
    let trimmed = text.trim_start();
    if trimmed
        .lines()
        .next()
        .is_some_and(|line| line.trim() == "mindmap")
    {
        return Some(text.trim().to_string());
    }
    None
}

/// Parse the `mindmap` subset this build understands: one root, two spaces of indentation per level,
/// and an optional shape wrapper around a node's text.
///
/// A depth jump is clamped to one level deeper than the line above, because Mermaid itself tolerates
/// it and a model that indents by four spaces for a grandchild should not lose the diagram.
pub fn parse_mindmap(source: &str) -> Result<MindNode, String> {
    let mut header = false;
    let mut nodes: Vec<(usize, String)> = Vec::new();
    // The root's own indentation is the baseline: Mermaid writes the root two spaces in, so a level is
    // two spaces past wherever the first node starts, not past the left margin.
    let mut base: Option<usize> = None;
    for raw in source.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with("%%") || trimmed.starts_with("::icon") {
            continue;
        }
        if !header {
            if trimmed != "mindmap" {
                return Err("AI did not return a Mermaid mindmap".into());
            }
            header = true;
            continue;
        }
        let mut indent = 0usize;
        for c in raw.chars() {
            match c {
                ' ' => indent += 1,
                '\t' => indent += 2,
                _ => break,
            }
        }
        let base = *base.get_or_insert(indent);
        let depth = indent.saturating_sub(base) / 2;
        nodes.push((depth, node_text(trimmed)));
    }
    // The last line of a streaming answer may be half a node; the previous preview stays while it
    // completes.
    if nodes
        .last()
        .is_some_and(|(_, text)| has_unclosed_wrapper(text))
    {
        nodes.pop();
    }
    if nodes.is_empty() {
        return Err("AI did not return a Mermaid mindmap".into());
    }
    let mut stack: Vec<MindNode> = Vec::new();
    let mut count = 0usize;
    for (depth, text) in nodes {
        if stack.is_empty() {
            stack.push(MindNode {
                text,
                children: Vec::new(),
            });
            count = 1;
            continue;
        }
        if depth == 0 {
            // One root per map: a second top-level line is the model starting over.
            continue;
        }
        count += 1;
        if count > MAX_MINDMAP_NODES {
            return Err("the mind map has too many nodes".into());
        }
        let want = depth.min(stack.len());
        while stack.len() > want {
            let Some(node) = stack.pop() else { break };
            if let Some(parent) = stack.last_mut() {
                parent.children.push(node);
            }
        }
        stack.push(MindNode {
            text,
            children: Vec::new(),
        });
    }
    while stack.len() > 1 {
        let Some(node) = stack.pop() else { break };
        if let Some(parent) = stack.last_mut() {
            parent.children.push(node);
        }
    }
    stack
        .pop()
        .ok_or_else(|| "AI did not return a Mermaid mindmap".to_string())
}

/// What a node says, from the line the model wrote.
///
/// Mermaid's node syntax is an id followed by an optional shape: `root((Robot))`, `wheels[Wheels]`,
/// `plan{Risk}`. The id is the node's name in the source, not its text, so the wrapper's contents are
/// what is drawn; a line with no wrapper is its own text. The reversed round forms Mermaid also accepts
/// (`)text(`, `))text((`) are stripped the same way.
fn node_text(raw: &str) -> String {
    for (open, close) in [("))", "(("), (")", "(")] {
        if let Some(inner) = raw
            .strip_prefix(open)
            .and_then(|rest| rest.strip_suffix(close))
        {
            let inner = inner.trim();
            if !inner.is_empty() {
                return inner.to_string();
            }
        }
    }
    for (open, close) in [
        ("((", "))"),
        ("{{", "}}"),
        ("[", "]"),
        ("{", "}"),
        ("(", ")"),
    ] {
        if let Some(inner) = raw
            .find(open)
            .and_then(|start| raw[start..].strip_prefix(open))
            .and_then(|rest| rest.strip_suffix(close))
        {
            let inner = inner.trim();
            if !inner.is_empty() {
                return inner.to_string();
            }
        }
    }
    raw.to_string()
}

/// Whether a node's text opens a wrapper it has not closed yet, which is what a half-received line
/// looks like.
fn has_unclosed_wrapper(text: &str) -> bool {
    let mut depth = 0i32;
    for c in text.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth > 0
}

/// Which arrangement a map is drawn with. The model may name one in a `%%` line inside its Mermaid
/// block; a map that names none gets a sensible default, because the shape of the diagram is the
/// reader's first clue about what kind of thinking it holds.
///
/// The four are the four jobs a mind map does: brainstorm (radial), decompose (tree), explain a cause
/// (fishbone), compare two things (double bubble).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    /// 发散: the root at the centre, branches on rings, clockwise from the right.
    Radial,
    /// 分解: a decomposition, read left to right, one column per level.
    Tree,
    /// 因果: causes of one effect, hung off a horizontal spine.
    Fishbone,
    /// 对比: exactly two subjects and the attributes they share between them.
    Compare,
}

impl MapKind {
    pub fn name(self) -> &'static str {
        match self {
            MapKind::Radial => "radial",
            MapKind::Tree => "tree",
            MapKind::Fishbone => "fishbone",
            MapKind::Compare => "compare",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "radial" | "ring" => Some(MapKind::Radial),
            "tree" | "right" => Some(MapKind::Tree),
            "fishbone" | "cause" => Some(MapKind::Fishbone),
            "compare" | "comparison" | "bubble" => Some(MapKind::Compare),
            _ => None,
        }
    }
}

/// The arrangement an answer asked for, from a `%% sgraffito layout: <kind>` line in its Mermaid
/// block. A comment is invisible to Mermaid, so naming the layout leaves the block a valid mind map.
pub fn layout_directive(source: &str) -> Option<MapKind> {
    for line in source.lines() {
        let Some(rest) = line.trim().strip_prefix("%%") else {
            continue;
        };
        let Some(value) = rest.trim().strip_prefix("sgraffito layout:") else {
            continue;
        };
        return MapKind::from_name(value);
    }
    None
}

/// The arrangement a map that named none gets: a hierarchy three levels deep or more is a
/// decomposition and reads to the right; anything shallower is a brainstorm and goes on a ring.
pub fn inferred_kind(tree: &MindNode) -> MapKind {
    if levels(tree) >= 3 {
        MapKind::Tree
    } else {
        MapKind::Radial
    }
}

/// The arrangement a map may actually use. A comparison needs exactly two first-level nodes and a
/// fishbone needs something to hang on the spine, so a map that cannot be drawn that way falls back to
/// the ring instead of being refused: the answer is still a mind map, just not the shape it asked for.
pub fn fit_kind(kind: MapKind, tree: &MindNode) -> MapKind {
    match kind {
        MapKind::Compare if tree.children.len() != 2 => MapKind::Radial,
        MapKind::Fishbone if tree.children.is_empty() => MapKind::Radial,
        other => other,
    }
}

/// A node as it is drawn: its box, its place, and the nodes it hangs from. Most layouts have one
/// parent; a comparison's shared attribute hangs from both subjects.
struct Placed {
    id: u64,
    parents: Vec<u64>,
    centre: [f32; 2],
    size: [f32; 2],
    text: String,
}

/// The diagram's box padding around its text, the ring step between depths, and the gaps the other
/// three arrangements use.
const NODE_TEXT_SIZE: f32 = 16.0;
const NODE_PADDING: f32 = 10.0;
const NODE_MIN_WIDTH: f32 = 120.0;
const NODE_MAX_WIDTH: f32 = 200.0;
const NODE_MIN_HEIGHT: f32 = 40.0;
const NODE_STROKE: f32 = 2.0;
const EDGE_STROKE: f32 = 1.5;
const RING_STEP: f32 = 190.0;
/// The gap between the columns of a tree, and between the rows of one.
const TREE_COLUMN_GAP: f32 = 44.0;
const TREE_ROW_GAP: f32 = 22.0;
/// How far a fishbone's bones rise off its spine, how far apart they sit, and how far their own
/// children step away from them.
const FISH_RISE: f32 = 120.0;
const FISH_SPACING: f32 = 210.0;
const FISH_STEP: f32 = 66.0;
/// The widest a comparison's columns may stand apart, and how much of the room they use.
const COMPARE_COLUMN: f32 = 300.0;

/// The box a node's text needs: the natural width, clamped, then the height the wrapped text takes.
fn node_size(text: &str, measure: &mut TextMeasure<'_>) -> [f32; 2] {
    let natural = measure(text, NODE_TEXT_SIZE, None);
    let width = (natural.0 + 2.0 * NODE_PADDING).clamp(NODE_MIN_WIDTH, NODE_MAX_WIDTH);
    let wrap = (width - 2.0 * NODE_PADDING).max(1.0);
    let wrapped = measure(text, NODE_TEXT_SIZE, Some(wrap));
    [width, (wrapped.1 + 2.0 * NODE_PADDING).max(NODE_MIN_HEIGHT)]
}

/// Lay `tree` out as `kind` about `anchor`.
///
/// `measure` is the renderer's own text measurement, so the boxes fit the glyphs rather than an
/// estimate. Every node is a rectangle (the document has no rounded box), every parent-child link is a
/// line attached to both ends, and one horizontal text box labels each node — a label is never turned,
/// because a turned label is unreadable at the angle a branch happens to have.
pub fn layout_mindmap(
    kind: MapKind,
    tree: &MindNode,
    anchor: [f32; 2],
    room: f32,
    color: &str,
    measure: &mut TextMeasure<'_>,
) -> Board {
    let kind = fit_kind(kind, tree);
    let anchor = origin(kind, anchor, room);
    let placed = match kind {
        MapKind::Radial => radial_place(tree, anchor, measure),
        MapKind::Tree => tree_place(tree, anchor, measure),
        MapKind::Fishbone => return fishbone_board(tree, anchor, color, measure),
        MapKind::Compare => compare_place(tree, anchor, room, measure)
            .unwrap_or_else(|| radial_place(tree, anchor, measure)),
    };
    assemble(&placed, color, measure)
}

/// Where a diagram starts inside the room the panel leaves: a ring grows from the middle of the room,
/// while a decomposition or a cause-and-effect map runs rightward from its left edge and is allowed to
/// run on under the panel, because a wide tree cannot be made to fit a strip and the canvas pans.
fn origin(kind: MapKind, anchor: [f32; 2], room: f32) -> [f32; 2] {
    match kind {
        MapKind::Radial | MapKind::Compare => [anchor[0] + room / 2.0, anchor[1]],
        MapKind::Tree | MapKind::Fishbone => [anchor[0] + 24.0, anchor[1]],
    }
}

/// The board for a set of placed nodes: the links first, so the nodes are drawn over them, then each
/// box with its label centred on it.
fn assemble(placed: &[Placed], color: &str, measure: &mut TextMeasure<'_>) -> Board {
    let mut board = Board::default();
    let mut next = placed.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    for node in placed {
        for parent_id in &node.parents {
            let Some(parent) = placed.iter().find(|other| other.id == *parent_id) else {
                continue;
            };
            push_edge(
                &mut board,
                next,
                parent.centre,
                node.centre,
                *parent_id,
                node.id,
                color,
            );
            next += 1;
        }
    }
    for node in placed {
        push_node(&mut board, node, color, measure);
    }
    // The stored endpoints are the node centres; the attachments are what put the lines on the
    // outlines. This is the same call the document makes when it loads a board.
    board.sync_connectors();
    board
}

fn push_edge(
    board: &mut Board,
    id: u64,
    from: [f32; 2],
    to: [f32; 2],
    from_id: u64,
    to_id: u64,
    color: &str,
) {
    board.shapes.push(Shape {
        kind: ShapeKind::Line,
        color: color.to_string(),
        width: EDGE_STROKE,
        from,
        to,
        rotate: 0.0,
        id,
        points: Vec::new(),
        roundness: None,
        attach: [
            Some(Attachment {
                shape: from_id,
                at: unit(sub(to, from)),
            }),
            Some(Attachment {
                shape: to_id,
                at: unit(sub(from, to)),
            }),
        ],
        fill: None,
    });
}

fn push_node(board: &mut Board, node: &Placed, color: &str, measure: &mut TextMeasure<'_>) {
    board.shapes.push(Shape {
        kind: ShapeKind::Rect,
        color: color.to_string(),
        width: NODE_STROKE,
        from: [
            node.centre[0] - node.size[0] / 2.0,
            node.centre[1] - node.size[1] / 2.0,
        ],
        to: [
            node.centre[0] + node.size[0] / 2.0,
            node.centre[1] + node.size[1] / 2.0,
        ],
        rotate: 0.0,
        id: node.id,
        points: Vec::new(),
        roundness: None,
        attach: [None, None],
        fill: None,
    });
    let wrap = (node.size[0] - 2.0 * NODE_PADDING).max(1.0);
    let (width, height) = measure(&node.text, NODE_TEXT_SIZE, Some(wrap));
    board.texts.push(TextItem {
        x: node.centre[0] - width / 2.0,
        y: node.centre[1] - height / 2.0,
        color: color.to_string(),
        size: NODE_TEXT_SIZE,
        text: node.text.clone(),
        link: None,
        container: Some(node.id),
    });
}

/// 发散: the root at the centre, one ring per depth, each child holding an angular slice proportional
/// to how much of the map hangs below it. Every slice runs the same way — children are laid out in
/// source order, clockwise from the right — so the eye has one direction to follow.
fn radial_place(tree: &MindNode, anchor: [f32; 2], measure: &mut TextMeasure<'_>) -> Vec<Placed> {
    let mut placed = Vec::new();
    let mut next = 1u64;
    ring(
        tree,
        0,
        0.0,
        std::f32::consts::TAU,
        anchor,
        &mut next,
        measure,
        &mut placed,
        None,
    );
    placed
}

#[allow(clippy::too_many_arguments)]
fn ring(
    node: &MindNode,
    depth: usize,
    start: f32,
    end: f32,
    anchor: [f32; 2],
    next: &mut u64,
    measure: &mut TextMeasure<'_>,
    out: &mut Vec<Placed>,
    parent: Option<u64>,
) -> u64 {
    let id = *next;
    *next += 1;
    let angle = (start + end) / 2.0;
    let radius = RING_STEP * depth as f32;
    let centre = [
        anchor[0] + radius * angle.cos(),
        anchor[1] + radius * angle.sin(),
    ];
    out.push(Placed {
        id,
        parents: parent.into_iter().collect(),
        centre,
        size: node_size(&node.text, measure),
        text: node.text.clone(),
    });
    let total: u32 = node.children.iter().map(leaves).sum();
    let mut from = start;
    for child in &node.children {
        let share = (end - start) * leaves(child) as f32 / total as f32;
        ring(
            child,
            depth + 1,
            from,
            from + share,
            anchor,
            next,
            measure,
            out,
            Some(id),
        );
        from += share;
    }
    id
}

/// 分解: one column per level, read left to right, each parent centred on the band its children occupy.
fn tree_place(tree: &MindNode, anchor: [f32; 2], measure: &mut TextMeasure<'_>) -> Vec<Placed> {
    let depth = levels(tree);
    let mut column = vec![0.0f32; depth];
    measure_widths(tree, 0, &mut column, measure);
    // Each column starts where the widest node of the column before it ended.
    let mut xs = Vec::with_capacity(depth);
    let mut x = anchor[0];
    for width in &column {
        xs.push(x);
        x += width + TREE_COLUMN_GAP;
    }
    let total = subtree_height(tree, measure);
    let mut placed = Vec::new();
    let mut next = 1u64;
    place_tree(
        tree,
        0,
        anchor[1] - total / 2.0,
        &xs,
        &mut next,
        measure,
        &mut placed,
        None,
    );
    placed
}

fn levels(node: &MindNode) -> usize {
    1 + node.children.iter().map(levels).max().unwrap_or(0)
}

fn measure_widths(
    node: &MindNode,
    depth: usize,
    column: &mut [f32],
    measure: &mut TextMeasure<'_>,
) {
    let width = node_size(&node.text, measure)[0];
    column[depth] = column[depth].max(width);
    for child in &node.children {
        measure_widths(child, depth + 1, column, measure);
    }
}

/// The vertical room a subtree needs: its own box, or its children's bands and the gaps between them.
fn subtree_height(node: &MindNode, measure: &mut TextMeasure<'_>) -> f32 {
    let own = node_size(&node.text, measure)[1];
    if node.children.is_empty() {
        return own;
    }
    let children: f32 = node
        .children
        .iter()
        .map(|child| subtree_height(child, measure))
        .sum::<f32>()
        + TREE_ROW_GAP * (node.children.len() - 1) as f32;
    own.max(children)
}

#[allow(clippy::too_many_arguments)]
fn place_tree(
    node: &MindNode,
    depth: usize,
    top: f32,
    xs: &[f32],
    next: &mut u64,
    measure: &mut TextMeasure<'_>,
    out: &mut Vec<Placed>,
    parent: Option<u64>,
) -> u64 {
    let id = *next;
    *next += 1;
    let band = subtree_height(node, measure);
    let size = node_size(&node.text, measure);
    out.push(Placed {
        id,
        parents: parent.into_iter().collect(),
        centre: [xs[depth] + size[0] / 2.0, top + band / 2.0],
        size,
        text: node.text.clone(),
    });
    if node.children.is_empty() {
        return id;
    }
    let children: f32 = node
        .children
        .iter()
        .map(|child| subtree_height(child, measure))
        .sum::<f32>()
        + TREE_ROW_GAP * (node.children.len() - 1) as f32;
    let mut y = top + (band - children) / 2.0;
    for child in &node.children {
        place_tree(child, depth + 1, y, xs, next, measure, out, Some(id));
        y += subtree_height(child, measure) + TREE_ROW_GAP;
    }
    id
}

/// 因果: the effect at the left of a spine, its causes hung off it above and below, and their own
/// children stepping away from the spine on the same side. The spine and the bones are drawn, not
/// implied: the effect is attached to the spine's near end, each cause to the far end of its bone.
fn fishbone_board(
    tree: &MindNode,
    anchor: [f32; 2],
    color: &str,
    measure: &mut TextMeasure<'_>,
) -> Board {
    let mut board = Board::default();
    let mut next = 1u64;
    let root_size = node_size(&tree.text, measure);
    let root = Placed {
        id: next,
        parents: Vec::new(),
        centre: anchor,
        size: root_size,
        text: tree.text.clone(),
    };
    next += 1;
    let spine_start = [anchor[0] + root_size[0] / 2.0, anchor[1]];
    let bones = tree.children.len().max(1);
    let spine_end = [
        spine_start[0] + FISH_SPACING * bones as f32 - FISH_SPACING * 0.45,
        anchor[1],
    ];
    // The spine hangs from the effect; its far end is free, which is what a spine is.
    board.shapes.push(Shape {
        kind: ShapeKind::Line,
        color: color.to_string(),
        width: EDGE_STROKE,
        from: spine_start,
        to: spine_end,
        rotate: 0.0,
        id: next,
        points: Vec::new(),
        roundness: None,
        attach: [
            Some(Attachment {
                shape: root.id,
                at: unit(sub(spine_end, spine_start)),
            }),
            None,
        ],
        fill: None,
    });
    next += 1;
    for (index, cause) in tree.children.iter().enumerate() {
        // Above, below, above… so consecutive causes never sit on top of each other.
        let side = if index % 2 == 0 { -1.0 } else { 1.0 };
        let along = spine_start[0] + FISH_SPACING * (index as f32 + 1.0);
        let bone_end = [along + FISH_RISE * 0.42, anchor[1] + side * FISH_RISE];
        let cause_size = node_size(&cause.text, measure);
        let cause_id = next;
        next += 1;
        let cause_centre = [
            bone_end[0] + cause_size[0] / 2.0,
            bone_end[1] + side * cause_size[1] / 2.0,
        ];
        // The bone: from a point on the spine to the cause's box.
        board.shapes.push(Shape {
            kind: ShapeKind::Line,
            color: color.to_string(),
            width: EDGE_STROKE,
            from: [along, anchor[1]],
            to: cause_centre,
            rotate: 0.0,
            id: next,
            points: Vec::new(),
            roundness: None,
            attach: [
                None,
                Some(Attachment {
                    shape: cause_id,
                    at: unit([along - cause_centre[0], anchor[1] - cause_centre[1]]),
                }),
            ],
            fill: None,
        });
        next += 1;
        push_node(
            &mut board,
            &Placed {
                id: cause_id,
                parents: Vec::new(),
                centre: cause_centre,
                size: cause_size,
                text: cause.text.clone(),
            },
            color,
            measure,
        );
        for (step, child) in cause.children.iter().enumerate() {
            let child_size = node_size(&child.text, measure);
            let centre = [
                cause_centre[0] + (step as f32 + 1.0) * FISH_STEP * 0.55,
                cause_centre[1] + side * (step as f32 + 1.0) * FISH_STEP,
            ];
            let child_id = next;
            next += 1;
            push_edge(
                &mut board,
                next,
                cause_centre,
                centre,
                cause_id,
                child_id,
                color,
            );
            next += 1;
            push_node(
                &mut board,
                &Placed {
                    id: child_id,
                    parents: Vec::new(),
                    centre,
                    size: child_size,
                    text: child.text.clone(),
                },
                color,
                measure,
            );
        }
    }
    push_node(&mut board, &root, color, measure);
    board.sync_connectors();
    board
}

/// 对比: the two subjects side by side, what they share between them, and what each has of its own on
/// its own side. `None` when the tree is not two subjects, so `layout_mindmap` can fall back.
fn compare_place(
    tree: &MindNode,
    anchor: [f32; 2],
    room: f32,
    measure: &mut TextMeasure<'_>,
) -> Option<Vec<Placed>> {
    let mut subjects = tree.children.iter();
    let left = subjects.next()?;
    let right = subjects.next()?;
    if subjects.next().is_some() {
        return None;
    }
    let shared = |a: &MindNode, b: &MindNode| -> Vec<String> {
        let names: Vec<String> = a.children.iter().map(|c| c.text.to_lowercase()).collect();
        b.children
            .iter()
            .filter(|c| names.contains(&c.text.to_lowercase()))
            .map(|c| c.text.clone())
            .collect()
    };
    let common = shared(left, right);
    fn unique<'a>(node: &'a MindNode, common: &[String]) -> Vec<&'a MindNode> {
        node.children
            .iter()
            .filter(|child| !common.iter().any(|c| c.eq_ignore_ascii_case(&child.text)))
            .collect()
    }
    let mut placed = Vec::new();
    let mut next = 1u64;
    let root_id = next;
    next += 1;
    let root_size = node_size(&tree.text, measure);
    let left_size = node_size(&left.text, measure);
    let right_size = node_size(&right.text, measure);
    // The subjects sit level with each other, the topic above them, and the shared attributes between.
    let subject_y = anchor[1] - 40.0;
    // The subjects stand as far apart as the room allows, never wider than a double bubble reads.
    let column = (room * 0.34).clamp(130.0, COMPARE_COLUMN);
    let left_centre = [anchor[0] - column, subject_y];
    let right_centre = [anchor[0] + column, subject_y];
    let left_id = next;
    next += 1;
    let right_id = next;
    next += 1;
    placed.push(Placed {
        id: left_id,
        parents: vec![root_id],
        centre: left_centre,
        size: left_size,
        text: left.text.clone(),
    });
    placed.push(Placed {
        id: right_id,
        parents: vec![root_id],
        centre: right_centre,
        size: right_size,
        text: right.text.clone(),
    });
    placed.insert(
        0,
        Placed {
            id: root_id,
            parents: Vec::new(),
            centre: [anchor[0], subject_y - 220.0],
            size: root_size,
            text: tree.text.clone(),
        },
    );
    let mut stack =
        |nodes: &[&MindNode], x: f32, parents: &[u64], next: &mut u64, placed: &mut Vec<Placed>| {
            let mut y = subject_y + 150.0;
            for node in nodes {
                let size = node_size(&node.text, measure);
                let id = *next;
                *next += 1;
                placed.push(Placed {
                    id,
                    parents: parents.to_vec(),
                    centre: [x, y + size[1] / 2.0],
                    size,
                    text: node.text.clone(),
                });
                y += size[1] + TREE_ROW_GAP;
            }
        };
    stack(
        &unique(left, &common),
        anchor[0] - column,
        &[left_id],
        &mut next,
        &mut placed,
    );
    stack(
        &unique(right, &common),
        anchor[0] + column,
        &[right_id],
        &mut next,
        &mut placed,
    );
    let shared_nodes: Vec<MindNode> = common
        .iter()
        .map(|text| MindNode {
            text: text.clone(),
            children: Vec::new(),
        })
        .collect();
    let shared_refs: Vec<&MindNode> = shared_nodes.iter().collect();
    stack(
        &shared_refs,
        anchor[0],
        &[left_id, right_id],
        &mut next,
        &mut placed,
    );
    Some(placed)
}

/// How much of the map hangs below a node; a leaf owns one slice.
fn leaves(node: &MindNode) -> u32 {
    if node.children.is_empty() {
        1
    } else {
        node.children.iter().map(leaves).sum()
    }
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn unit(v: [f32; 2]) -> [f32; 2] {
    let length = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if length <= f32::EPSILON {
        [0.0, 0.0]
    } else {
        [v[0] / length, v[1] / length]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one property a sandboxed request has to keep: the request is in curl's configuration on
    /// stdin, so no argv and no environment of any process carries the credential.
    #[test]
    fn the_credential_is_only_in_the_configuration_on_stdin() {
        let settings = AiSettings {
            endpoint: "https://example.test/openai/v1/chat/completions".into(),
            model: "m".into(),
            key: "sk-secret".into(),
            timeout: Duration::from_secs(60),
        };
        let args: Vec<String> = sandbox_args()
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|arg| arg == "--clearenv"),
            "the sandbox inherits nothing: {args:?}"
        );
        assert!(args.iter().all(|arg| !arg.contains("sk-secret")));
        assert!(args.iter().all(|arg| !arg.contains("example.test")));

        let config = curl_config(&settings, "ses_test", "{\"stream\":true}");
        assert!(config.contains("url = \"https://example.test/openai/v1/chat/completions\""));
        assert!(config.contains("Authorization: Bearer sk-secret"));
        assert!(config.contains("x-opencode-session: ses_test"));
        assert!(config.contains("data-binary = \"{\\\"stream\\\":true}\""));
        assert!(config.contains("max-time = 60"));
        assert!(
            config.contains("__SGRAFFITO_HTTP__%{response_code}"),
            "the status line the reader parses: {config}"
        );
        // And the trailing curl arguments say the configuration comes from stdin.
        assert_eq!(CURL_ARGS, ["-N", "-K", "-"]);
    }

    /// The reader's own path, with no child process: a gateway that answers 401 for want of a key has
    /// to come back as the sentence the user can act on, status line and all.
    #[test]
    fn a_401_becomes_an_error_that_names_the_missing_key() {
        use std::io::Cursor;
        let (tx, rx) = mpsc::channel();
        let (ping, _source) = calloop::ping::make_ping().unwrap();
        let body = "{\"errors\":[{\"type\":\"authentication_error\",\"message\":\"OpenCode Go requires an API key\"}]}\n\
                    __SGRAFFITO_HTTP__401\n";
        read_stream(Cursor::new(body), tx, ping.clone(), false);
        let events: Vec<AiEvent> = rx.try_iter().collect();
        assert_eq!(events.len(), 1, "{events:?}");
        let AiEvent::Error(message) = &events[0] else {
            panic!("expected an error, got {events:?}");
        };
        assert!(message.starts_with("HTTP 401: "), "{message}");
        assert!(message.contains("requires an API key"), "{message}");
        assert!(message.contains("no API key was sent"), "{message}");
    }

    #[test]
    fn a_stream_of_deltas_ends_in_done() {
        use std::io::Cursor;
        let (tx, rx) = mpsc::channel();
        let (ping, _source) = calloop::ping::make_ping().unwrap();
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                    data: [DONE]\n\
                    __SGRAFFITO_HTTP__200\n";
        read_stream(Cursor::new(body), tx, ping, true);
        let events: Vec<AiEvent> = rx.try_iter().collect();
        assert_eq!(
            events,
            vec![AiEvent::Delta("hi".into()), AiEvent::Done(200)]
        );
    }

    #[test]
    fn a_keyless_request_sends_no_authorization_header() {
        let settings = AiSettings {
            endpoint: "http://127.0.0.1:8765".into(),
            model: "m".into(),
            key: String::new(),
            timeout: Duration::from_secs(5),
        };
        let config = curl_config(&settings, "ses_x", "{}");
        assert!(!config.contains("Authorization"), "{config}");
    }

    #[test]
    fn the_configuration_escapes_what_curls_parser_reads() {
        let settings = AiSettings {
            endpoint: "https://x/openai/v1/chat/completions".into(),
            model: "m".into(),
            key: "a\\b\"c".into(),
            timeout: Duration::from_secs(5),
        };
        let config = curl_config(&settings, "ses_x", "line\nquote\"");
        assert!(
            config.contains("Authorization: Bearer a\\\\b\\\"c"),
            "{config}"
        );
        assert!(
            config.contains("data-binary = \"line\\nquote\\\"\""),
            "{config}"
        );
    }

    #[test]
    fn a_proxy_is_taken_in_curls_own_order() {
        assert_eq!(
            proxy_config(
                Some("http://https-proxy"),
                Some("http://all"),
                Some("http://http"),
                None
            ),
            "proxy = \"http://https-proxy\"\n"
        );
        assert_eq!(
            proxy_config(
                None,
                Some("http://all"),
                Some("http://http"),
                Some("localhost, 127.0.0.1")
            ),
            "proxy = \"http://all\"\nnoproxy = \"localhost, 127.0.0.1\"\n"
        );
        assert_eq!(proxy_config(Some("  "), None, None, None), "");
    }

    /// A request names the model and streams, and carries the prompt as the last user turn.
    #[test]
    fn the_body_asks_for_a_stream_and_carries_the_prompt() {
        let settings = AiSettings {
            endpoint: "http://127.0.0.1:1/openai/v1/chat/completions".into(),
            model: "test-model".into(),
            key: String::new(),
            timeout: Duration::from_secs(30),
        };
        let messages = vec![
            Message {
                role: Role::User,
                text: "first".into(),
                streaming: false,
            },
            Message {
                role: Role::Assistant,
                text: "answer".into(),
                streaming: false,
            },
            Message {
                role: Role::User,
                text: "second".into(),
                streaming: false,
            },
            Message {
                role: Role::Assistant,
                text: String::new(),
                streaming: true,
            },
        ];
        let body = request_body(&settings, &messages, "second");
        assert_eq!(body["stream"], serde_json::json!(true));
        assert_eq!(body["model"], serde_json::json!("test-model"));
        let turns = body["messages"].as_array().unwrap();
        assert_eq!(turns[0]["role"], serde_json::json!("system"));
        assert_eq!(
            turns.last().unwrap()["content"],
            serde_json::json!("second")
        );
        // The empty answer being streamed is not sent back as history.
        assert!(
            !turns
                .iter()
                .any(|turn| turn["role"] == serde_json::json!("assistant")
                    && turn["content"] == serde_json::json!(""))
        );
    }
}
