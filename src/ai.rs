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
    AiMessageView, AiView, Attachment, Board, Rect, Seed, Shape, ShapeKind, TextBuffer, TextItem,
};
use crate::config::{Config, DEFAULT_AI_MODEL};

/// What the model is asked for. One fenced `mindmap`, nothing after it: the parser reads the last
/// fenced block, so a correction after an earlier attempt replaces it rather than adding to it.
pub const SYSTEM_PROMPT: &str = "\
You create Mermaid diagrams for the sgraffito drawing board.
Answer in the user's language. Keep prose to at most two short sentences.
Then output exactly one fenced code block with language `mermaid`.
Use a `mindmap` unless the content is a process, a history or a 2x2 comparison: then a tree-shaped
`flowchart` (one start, no node with two parents; `flowchart TD` reads downwards, `flowchart LR` to the
right), a `timeline`, or a `quadrantChart` are read too.
The block starts with `mindmap`, then one comment line naming the arrangement:
  %% sgraffito layout: radial | tree | tree-down | fishbone | compare
Use two spaces per indentation level. One root and at most 40 nodes. Do not add any text after the block.

Choose the arrangement by what the map is for:
- radial: brainstorming a topic, no fixed order (发散).
- tree: decomposing something into parts, two levels or more below the root (分解).
- tree-down: a decomposition that is read top to bottom — an organisation, a process, a chain of steps
  (流程/组织). Its levels are rows rather than columns.
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

/// What `Return` sends when the input is empty and the board has a selection: the selection is the
/// request, so naming the act is enough.
pub const EXPAND_PROMPT: &str = "Expand the selected content into a mind map.";

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
    /// How long the answer was when the preview was last laid out. A stream that has gone quiet must
    /// not be re-parsed: the parse and the layout cost real work, and an endpoint that never closes
    /// its stream would pay it several times a second forever.
    pub preview_len: usize,
    /// What this request was asked to expand, when it was sent with a selection: the answer is laid out
    /// where that selection stood and takes its place.
    pub seed: Option<Seed>,
    /// Where this answer's own box was put on the board, in canvas coordinates. A map grows as the answer
    /// streams, and a fresh search on every rebuild would make it hop across the board; it keeps its
    /// corner while that corner still fits.
    pub place: Option<[f32; 2]>,
    /// The arrangement the diagram on the board was drawn with: what a view switch counts from.
    pub view: Option<MapKind>,
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
            preview_len: 0,
            seed: None,
            place: None,
            view: None,
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

/// Which of a board's conversations the panel shows. The board has one of its own, opened by `I`; the
/// card under a selection opens a second, whose subject is what is selected. They share the panel and
/// nothing else: no transcript, no draft, no diagram and no error crosses between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// The board's own conversation.
    Board,
    /// The selection's conversation, which the action card's `ai` opens and which expands in place.
    Selection,
}

/// The AI panel: one window, showing one conversation at a time.
///
/// The window is one and its conversations are many: one per board, plus one per board for the
/// selection. The transcript, the draft, the error, the scroll and the diagram belong to a
/// conversation, so switching boards or scopes switches which one is shown and nothing else: no
/// conversation can read another's context or replace another's diagram.
pub struct Chat {
    pub open: bool,
    /// The output the panel is drawn on. One panel, on one screen.
    pub output: Option<u32>,
    /// Whether the input line owns the keyboard.
    pub focus: bool,
    /// The board the shown conversation belongs to, and the one an answer is placed on.
    board: String,
    /// Which of that board's conversations is shown.
    scope: Scope,
    /// `(board, scope)` as one key, so the map is read without allocating a string per frame.
    key: (String, Scope),
    sessions: std::collections::BTreeMap<(String, Scope), Session>,
    /// The session header the Go endpoint wants. One id per process, so a follow-up continues it.
    pub api_session: String,
    /// A press that went down inside the panel owns the pointer until it is released.
    pub pointer_captured: bool,
    /// When the panel last opened: its card slides into place over the rail's own transition, and this
    /// is the clock that drives it. `None` until it has opened once.
    pub open_since: Option<Instant>,
    /// The conversation an answer is still arriving for. A delta lands there, never in whichever
    /// conversation happens to be on screen when it arrives.
    streaming: Option<(String, Scope)>,
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
        sessions.insert((String::new(), Scope::Board), Session::new());
        Self {
            open: false,
            output: None,
            focus: false,
            board: String::new(),
            scope: Scope::Board,
            key: (String::new(), Scope::Board),
            sessions,
            api_session: session_id(),
            pointer_captured: false,
            open_since: None,
            streaming: None,
            generation: None,
            rx: None,
        }
    }

    /// The board the shown conversation belongs to, which is where its answer is placed.
    pub fn board(&self) -> &str {
        &self.board
    }

    /// Which of the board's conversations is shown.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// The conversation shown. A scope that has never been asked anything has an empty one, created on
    /// first sight so the panel can always draw something.
    pub fn session(&self) -> &Session {
        self.sessions
            .get(&self.key)
            .expect("a shown conversation always exists")
    }

    pub fn session_mut(&mut self) -> &mut Session {
        self.sessions.entry(self.key.clone()).or_default()
    }

    /// Show `board`'s own conversation. Returns whether what is shown changed, so the caller can stop a
    /// request that belonged to the conversation being left.
    pub fn enter(&mut self, board: &str) -> bool {
        self.enter_scope(board, Scope::Board)
    }

    /// Show one of `board`'s conversations.
    pub fn enter_scope(&mut self, board: &str, scope: Scope) -> bool {
        if self.board == board && self.scope == scope {
            self.sessions.entry(self.key.clone()).or_default();
            return false;
        }
        // A preedit is a composition in flight, not content: it belongs to the conversation it was
        // begun in.
        if let Some(previous) = self.sessions.get_mut(&self.key) {
            previous.preedit.clear();
        }
        self.board = board.to_string();
        self.scope = scope;
        self.key = (self.board.clone(), scope);
        self.sessions.entry(self.key.clone()).or_default();
        true
    }

    /// Forget a board's conversations: the board itself is gone. A panel still showing that board keeps
    /// an empty conversation rather than a hole, so the window can always be drawn.
    pub fn forget(&mut self, board: &str) {
        for scope in [Scope::Board, Scope::Selection] {
            self.sessions.remove(&(board.to_string(), scope));
        }
        if self.board == board {
            self.sessions.insert(self.key.clone(), Session::new());
        }
    }

    /// Clear the shown conversation's live preview: the answer it belonged to is not coming.
    pub fn clear_preview(&mut self) {
        let session = self.session_mut();
        session.preview = None;
        // The place the answer was to take is part of the preview, not of the conversation: a request
        // that failed or was committed leaves no instruction behind for the next one.
        session.seed = None;
        session.place = None;
    }

    /// Record a failure against `board`'s own conversation, whether or not that board is on screen.
    pub fn fail(&mut self, board: &str, reason: &str) {
        let session = self
            .sessions
            .entry((board.to_string(), Scope::Board))
            .or_default();
        session.preview = None;
        session.error = Some(reason.to_string());
    }

    /// Record a failure against the conversation that asked for the answer, which is not always the one
    /// on screen.
    pub fn fail_asking(&mut self, reason: &str) {
        let key = self.streaming.clone().unwrap_or_else(|| self.key.clone());
        let session = self.sessions.entry(key).or_default();
        session.preview = None;
        session.error = Some(reason.to_string());
    }

    /// The board an answer is arriving for, if one is.
    pub fn streaming_board(&self) -> Option<&str> {
        self.streaming.as_ref().map(|(board, _)| board.as_str())
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
            streaming: self.streaming.as_ref().is_some_and(|key| *key == self.key),
            error: session.error.clone(),
            scroll: session.scroll,
            burst: session.burst_drawn,
            grow: 1.0,
            view: session.view.map(|kind| kind.name()),
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
        let seed = session.seed.as_ref().map(|seed| seed.text.as_str());
        let body = request_body(&settings, &session.messages, &prompt, seed).to_string();
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
        self.streaming = Some(self.key.clone());
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
        // The answer belongs to the conversation it was asked in, even if another is on screen now.
        let key = self.streaming.clone().unwrap_or_else(|| self.key.clone());
        let session = self.sessions.entry(key).or_default();
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
            self.streaming = None;
            session.stop_streaming();
        }
        changed
    }

    /// Stop the request in flight, keeping what has already arrived. Closing the panel is one caller;
    /// a failed or stopped request is another.
    pub fn abort(&mut self) {
        self.rx = None;
        self.generation = None;
        let key = self.streaming.take().unwrap_or_else(|| self.key.clone());
        if let Some(session) = self.sessions.get_mut(&key) {
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
/// The system turn: the mind-map rules, and the words of the selection the answer is to expand.
fn system_prompt(seed: Option<&str>) -> String {
    match seed.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => format!(
            "{SYSTEM_PROMPT}\n\nThe user selected this content on the board and wants it expanded in \
             place; the map's root is its subject:\n{text}"
        ),
        None => SYSTEM_PROMPT.to_string(),
    }
}

fn request_body(
    settings: &AiSettings,
    messages: &[Message],
    prompt: &str,
    seed: Option<&str>,
) -> Value {
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
    turns.push(json!({ "role": "system", "content": system_prompt(seed) }));
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

/// Which diagram syntax an answer's block is written in. The model names the kind of thinking; the
/// syntax says how it is written down, and the geometry is the same pipeline either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// A Mermaid `mindmap`: indentation is the structure.
    Mindmap,
    /// A `flowchart`/`graph`: arrows are the structure, and the direction token is the reading direction.
    Flowchart,
    /// A `timeline`: sections in order, each with its events.
    Timeline,
    /// A `quadrantChart`: four named quadrants and the points that sit in them.
    Quadrant,
}

/// What an answer's block holds, after its syntax is read: the structure to draw, and the arrangement its
/// syntax implies, which the rules and the answer's own `%%` line may still overrule.
#[derive(Debug, Clone)]
pub struct Parsed {
    pub tree: MindNode,
    pub syntax: Syntax,
    pub view: Option<MapKind>,
}

/// Read a Mermaid block: a `mindmap`, a tree-shaped `flowchart`, a `timeline` or a `quadrantChart`. All
/// four become the one structure the layout understands — nodes and parents — so no geometry knows which
/// syntax produced it.
pub fn parse_diagram(source: &str) -> Result<Parsed, String> {
    let first = source
        .lines()
        .find(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("%%")
        })
        .map(|line| line.trim().to_string())
        .unwrap_or_default();
    let word = first.split_whitespace().next().unwrap_or_default();
    match word {
        "mindmap" => {
            let tree = parse_mindmap(source)?;
            let view = layout_directive(source).or(Some(inferred_kind(&tree)));
            Ok(Parsed {
                tree,
                syntax: Syntax::Mindmap,
                view,
            })
        }
        "flowchart" | "graph" => flowchart_tree(source, &first).map(|tree| Parsed {
            tree,
            syntax: Syntax::Flowchart,
            view: Some(flowchart_view(&first)),
        }),
        "timeline" => timeline_tree(source).map(|tree| Parsed {
            tree,
            syntax: Syntax::Timeline,
            view: Some(MapKind::Tree(Dir::Down)),
        }),
        "quadrantChart" => quadrant_tree(source).map(|tree| Parsed {
            tree,
            syntax: Syntax::Quadrant,
            view: Some(MapKind::Tree(Dir::Right)),
        }),
        _ => {
            Err("AI did not return a Mermaid mindmap, flowchart, timeline or quadrant chart".into())
        }
    }
}

/// The reading direction a flowchart names: `LR` reads right, everything else (top-down, bottom-up,
/// right-left) is drawn downwards, because that is the axis a chain of steps is written on.
fn flowchart_view(header: &str) -> MapKind {
    let mut words = header.split_whitespace();
    let _ = words.next();
    match words.next() {
        Some("LR") => MapKind::Tree(Dir::Right),
        _ => MapKind::Tree(Dir::Down),
    }
}

/// One end of a flowchart edge: its id, and the text it was given if any.
fn flowchart_end(raw: &str) -> (String, Option<String>) {
    let raw = raw.trim();
    let split = raw.find(['(', '[', '{', '>']).unwrap_or(raw.len());
    let (id, wrapper) = raw.split_at(split);
    let id = id.trim().to_string();
    let text = if wrapper.trim().is_empty() {
        None
    } else {
        let text = node_text(wrapper.trim());
        (!text.is_empty()).then_some(text)
    };
    (id, text)
}

/// A flowchart as a tree: the one node nothing points at is the root, every other node hangs from the
/// node its first arrow comes from. A graph with a merge (a node with two parents) is refused rather
/// than flattened, because a tree would draw a different diagram from the one written.
fn flowchart_tree(source: &str, header: &str) -> Result<MindNode, String> {
    let _ = header;
    let mut labels: std::collections::BTreeMap<String, String> = Default::default();
    let mut order: Vec<String> = Vec::new();
    let mut parents: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut note = |id: &str, text: &Option<String>| {
        if !labels.contains_key(id) {
            order.push(id.to_string());
            labels.insert(
                id.to_string(),
                text.clone().unwrap_or_else(|| id.to_string()),
            );
        } else if let Some(text) = text
            && !text.is_empty()
        {
            labels.insert(id.to_string(), text.clone());
        }
    };
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with("%%")
            || line == "end"
            || line.starts_with("subgraph")
            || line.starts_with("classDef")
            || line.starts_with("class ")
            || line.starts_with("style ")
            || line.starts_with("linkStyle")
            || line.starts_with("click ")
            || line.starts_with("direction ")
        {
            continue;
        }
        if line == "flowchart"
            || line.starts_with("flowchart ")
            || line == "graph"
            || line.starts_with("graph ")
        {
            continue;
        }
        // `A[One] --> B[Two] --> C[Three]`, with an optional edge label after the arrow.
        let Some((first, tail)) = line.split_once("-->") else {
            continue;
        };
        let (from_id, from_text) = flowchart_end(first);
        note(&from_id, &from_text);
        let mut cursor = tail;
        loop {
            let (to_id, to_text) = match cursor.split_once("-->") {
                Some((head, next)) => {
                    let (id, text) = flowchart_end(head);
                    cursor = next;
                    (id, text)
                }
                None => {
                    let head = cursor.split_once("|").map_or(cursor, |(head, _)| head);
                    let (id, text) = flowchart_end(head);
                    (id, text)
                }
            };
            note(&to_id, &to_text);
            parents.entry(to_id).or_default().push(from_id.clone());
            if !cursor.contains("-->") {
                break;
            }
        }
    }
    if labels.is_empty() {
        return Err("the flowchart has no arrows to draw".into());
    }
    let roots: Vec<&String> = order
        .iter()
        .filter(|id| !parents.contains_key(*id))
        .collect();
    if roots.len() != 1 {
        return Err("the flowchart needs exactly one start: draw a mind map instead".into());
    }
    if let Some((id, _)) = parents.iter().find(|(_, from)| from.len() > 1) {
        return Err(format!(
            "the flowchart joins back at '{id}', which a tree cannot draw"
        ));
    }
    fn build(
        id: &str,
        labels: &std::collections::BTreeMap<String, String>,
        parents: &std::collections::BTreeMap<String, Vec<String>>,
        count: &mut usize,
    ) -> Result<MindNode, String> {
        *count += 1;
        if *count > MAX_MINDMAP_NODES {
            return Err("the mind map has too many nodes".into());
        }
        let mut children = Vec::new();
        for (child, from) in parents {
            if from.first().is_some_and(|parent| parent == id) {
                children.push(build(child, labels, parents, count)?);
            }
        }
        Ok(MindNode {
            text: labels.get(id).cloned().unwrap_or_else(|| id.to_string()),
            children,
        })
    }
    let mut count = 0;
    let tree = build(roots[0], &labels, &parents, &mut count)?;
    Ok(tree)
}

/// A timeline as a tree: the title at the root, one child per section, the section's events under it.
fn timeline_tree(source: &str) -> Result<MindNode, String> {
    let mut title: Option<String> = None;
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<usize> = None;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with("%%")
            || line.starts_with("accTitle")
            || line.starts_with("accDescr")
        {
            continue;
        }
        if line == "timeline" {
            continue;
        }
        if let Some(rest) = line.strip_prefix("title ") {
            title = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("section ") {
            sections.push((rest.trim().to_string(), Vec::new()));
            current = Some(sections.len() - 1);
            continue;
        }
        let events: Vec<&str> = line
            .split(':')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        if let Some(index) = current {
            for event in events {
                if let Some(section) = sections.get_mut(index) {
                    section.1.push(event.to_string());
                }
            }
        } else {
            // An event before any section is a period of its own.
            for event in events {
                sections.push((event.to_string(), Vec::new()));
                current = Some(sections.len() - 1);
            }
        }
    }
    if sections.is_empty() {
        return Err("the timeline has no sections".into());
    }
    let body = sections
        .into_iter()
        .map(|(name, events)| MindNode {
            text: name,
            children: events
                .into_iter()
                .map(|text| MindNode {
                    text,
                    children: Vec::new(),
                })
                .collect(),
        })
        .collect();
    Ok(MindNode {
        text: title.unwrap_or_else(|| "Timeline".into()),
        children: body,
    })
}

/// A quadrant chart as a tree: the title at the root, the four quadrants under it, each holding the
/// points that fall in it. The axes' names are not drawn (the tree has no axis to put them on).
fn quadrant_tree(source: &str) -> Result<MindNode, String> {
    let mut title: Option<String> = None;
    let mut quadrants: [Option<String>; 4] = Default::default();
    let mut points: Vec<(String, usize)> = Vec::new();
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with("%%")
            || line == "quadrantChart"
            || line.starts_with("x-axis")
            || line.starts_with("y-axis")
            || line.starts_with("accTitle")
            || line.starts_with("accDescr")
        {
            continue;
        }
        if let Some(rest) = line.strip_prefix("title ") {
            title = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("quadrant-")
            && let Some((index, name)) = rest.split_once(' ')
        {
            let index: usize = index.trim().parse().unwrap_or(0);
            if (1..=4).contains(&index) {
                quadrants[index - 1] = Some(name.trim().to_string());
            }
            continue;
        }
        if let Some((name, coords)) = line.split_once(':') {
            let inside = coords.trim().trim_start_matches('[').trim_end_matches(']');
            let mut parts = inside
                .split(',')
                .map(|part| part.trim().parse::<f32>().ok());
            if let (Some(Some(x)), Some(Some(y))) = (parts.next(), parts.next()) {
                // Quadrant 1 is the top right, and the numbering runs counter-clockwise from it.
                let right = x >= 0.5;
                let top = y >= 0.5;
                let index = match (right, top) {
                    (true, true) => 0,
                    (false, true) => 1,
                    (false, false) => 2,
                    (true, false) => 3,
                };
                points.push((name.trim().to_string(), index));
            }
        }
    }
    if quadrants.iter().all(Option::is_none) && points.is_empty() {
        return Err("the quadrant chart has nothing in it".into());
    }
    let children = (0..4)
        .filter_map(|index| {
            let named = quadrants[index].clone().or_else(|| {
                points
                    .iter()
                    .any(|(_, at)| *at == index)
                    .then(|| format!("Q{}", index + 1))
            })?;
            let inside = points
                .iter()
                .filter(|(_, at)| *at == index)
                .map(|(name, _)| MindNode {
                    text: name.clone(),
                    children: Vec::new(),
                })
                .collect();
            Some(MindNode {
                text: named,
                children: inside,
            })
        })
        .collect();
    Ok(MindNode {
        text: title.unwrap_or_else(|| "Quadrants".into()),
        children,
    })
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
/// Which way a decomposition grows. A decomposition of a topic reads to the right, a column per
/// level; a decomposition of an organisation or a process reads downwards, a row per level, because a
/// chain of "then" is vertical. The nodes stay upright either way: only the layers swap axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dir {
    Right,
    Down,
}

impl Dir {
    /// A node's extent along the depth axis: the way its children grow away from it.
    fn along(self, size: [f32; 2]) -> f32 {
        match self {
            Dir::Right => size[0],
            Dir::Down => size[1],
        }
    }

    /// A node's extent across the depth axis: the way its siblings stand beside it.
    fn across(self, size: [f32; 2]) -> f32 {
        match self {
            Dir::Right => size[1],
            Dir::Down => size[0],
        }
    }

    /// A canvas point from a distance along the depth axis and a distance across it.
    fn point(self, along: f32, across: f32) -> [f32; 2] {
        match self {
            Dir::Right => [along, across],
            Dir::Down => [across, along],
        }
    }
}

/// The arrangements a map can be drawn with: the four jobs a mind map does, plus the direction a
/// decomposition reads in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    /// 发散: the root at the centre, branches on rings, clockwise from the right.
    Radial,
    /// 分解: a decomposition, one column per level to the right or one row per level down.
    Tree(Dir),
    /// 因果: causes of one effect, hung off a horizontal spine.
    Fishbone,
    /// 对比: exactly two subjects and the attributes they share between them.
    Compare,
}

impl MapKind {
    pub fn name(self) -> &'static str {
        match self {
            MapKind::Radial => "radial",
            MapKind::Tree(Dir::Right) => "tree",
            MapKind::Tree(Dir::Down) => "tree-down",
            MapKind::Fishbone => "fishbone",
            MapKind::Compare => "compare",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "radial" | "ring" => Some(MapKind::Radial),
            "tree" | "right" | "tree-right" => Some(MapKind::Tree(Dir::Right)),
            "down" | "tree-down" | "org" | "flow" => Some(MapKind::Tree(Dir::Down)),
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
        MapKind::Tree(Dir::Right)
    } else {
        MapKind::Radial
    }
}

/// The arrangements worth switching between for this structure, in the order a reader tries them: the
/// two readings of a decomposition, then the ring, then the semantic shapes it supports. Every one of
/// them is drawn from the same tree — switching asks the model nothing.
pub fn view_cycle(tree: &MindNode) -> Vec<MapKind> {
    let mut views = vec![
        MapKind::Tree(Dir::Right),
        MapKind::Tree(Dir::Down),
        MapKind::Radial,
    ];
    if tree.children.len() == 2 {
        views.push(MapKind::Compare);
    }
    if !tree.children.is_empty() {
        views.push(MapKind::Fishbone);
    }
    views
}

/// The arrangement after `current` in the cycle, wrapping. A view that is not in the cycle (the answer
/// named a shape this structure cannot take) starts at the top.
pub fn next_view(current: Option<MapKind>, tree: &MindNode) -> MapKind {
    let views = view_cycle(tree);
    match current.and_then(|kind| views.iter().position(|view| *view == kind)) {
        Some(index) => views[(index + 1) % views.len()],
        None => views[0],
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
/// How far a branch's boundary stands off the nodes it holds, and how much room its title keeps above
/// them: the title is a node of its own, standing inside the boundary's top edge.
const GROUP_PADDING: f32 = 18.0;
const GROUP_TITLE_GAP: f32 = 10.0;
const GROUP_STROKE: f32 = 1.5;
/// A boundary is drawn at a little over half the ink, thin: it reads as a boundary and not as another
/// node, and the edges it holds still carry the eye.
const GROUP_ALPHA: u8 = 0x88;
/// Edges are drawn through their bends as a smooth curve: Excalidraw's proportional roundness.
const EDGE_ROUNDNESS: u8 = 2;
/// How much one axis has to dominate the other before an edge is routed as an elbow rather than a
/// straight diagonal: root-to-subject at 45° is a diagonal, a stack of attributes is an elbow.
const ELBOW_RATIO: f32 = 1.6;
/// How far a decomposition's edge reaches sideways before it turns, as a fraction of the corridor it
/// crosses: at half, the curve keeps its S instead of bulging into a block.
const COLUMN_HANDLE: f32 = 0.5;
/// How many pieces a decomposition's S is sampled into. The curve is drawn through these points, so they
/// are the shape: a handful is enough for a curve that is smooth at both ends.
const COLUMN_SAMPLES: usize = 6;

/// A point on the cubic through `a`, `b`, `c` and `d`.
fn cubic(a: [f32; 2], b: [f32; 2], c: [f32; 2], d: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [
        w0 * a[0] + w1 * b[0] + w2 * c[0] + w3 * d[0],
        w0 * a[1] + w1 * b[1] + w2 * c[1] + w3 * d[1],
    ]
}
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
    let arrangement = match kind {
        MapKind::Radial => Arrangement {
            nodes: radial_place(tree, anchor, measure),
            groups: Vec::new(),
        },
        MapKind::Tree(dir) => tree_place(tree, anchor, measure, dir),
        MapKind::Fishbone => return fishbone_board(tree, anchor, color, measure),
        MapKind::Compare => {
            compare_place(tree, anchor, room, measure).unwrap_or_else(|| Arrangement {
                nodes: radial_place(tree, anchor, measure),
                groups: Vec::new(),
            })
        }
    };
    match kind {
        MapKind::Tree(dir) => assemble_routed(&arrangement, color, measure, Route::Branches(dir)),
        MapKind::Compare => assemble_routed(&arrangement, color, measure, Route::ByAxis),
        _ => assemble_routed(&arrangement, color, measure, Route::Direct),
    }
}

/// A laid-out map: its nodes, and the boxes that group a branch, drawn behind everything.
struct Arrangement {
    nodes: Vec<Placed>,
    /// One box per first-level branch that has children: the branch's own node stands inside its top
    /// edge as the title, and its descendants fill the rest. Nothing is stored about a group beyond the
    /// box: the title is a node like any other, so it moves, labels and attaches like one.
    groups: Vec<Rect>,
}

/// The middle of the free corridor between two nodes, measured from their edges rather than their
/// centres: an edge runs down the gap it is given, not through the wider node's box.
fn gap_mid(a_centre: f32, a_size: f32, b_centre: f32, b_size: f32) -> f32 {
    if b_centre >= a_centre {
        (a_centre + a_size / 2.0 + b_centre - b_size / 2.0) / 2.0
    } else {
        (a_centre - a_size / 2.0 + b_centre + b_size / 2.0) / 2.0
    }
}

/// How the edge between two nodes gets from one to the other. Every edge is drawn with curved roundness;
/// a route only decides where its bend points are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    /// Straight from one node to the other: a ring's spokes and a fishbone's ribs are already paths.
    Direct,
    /// Through the corridor between two layers of a decomposition, leaving the parent and landing on the
    /// child along the direction it grows: sideways when it grows to the right, downwards when it grows
    /// down.
    Branches(Dir),
    /// Along whichever axis the two nodes are furthest apart on: a comparison stacks vertically.
    ByAxis,
}

impl Route {
    /// The bend points between two node centres, in canvas coordinates.
    fn bends(self, parent: &Placed, child: &Placed) -> Vec<[f32; 2]> {
        let (from, to) = (parent.centre, child.centre);
        // A bend that lands on an end is no bend at all: two nodes in one column are joined straight.
        let real = |point: [f32; 2]| {
            (point[0] - from[0]).abs() + (point[1] - from[1]).abs() > 0.5
                && (point[0] - to[0]).abs() + (point[1] - to[1]).abs() > 0.5
        };
        let across = gap_mid(from[0], parent.size[0], to[0], child.size[0]);
        let down = gap_mid(from[1], parent.size[1], to[1], child.size[1]);
        let bends = match self {
            Route::Direct => Vec::new(),
            Route::Branches(dir) => {
                // The S a mind map draws: it leaves the parent's edge and lands on the child's edge along
                // the depth axis, one curve per edge. Siblings fan out of the same point instead of
                // sharing one corridor, which would draw the same line three times and knot it.
                let (from_along, to_along) = (dir.along(parent.size), dir.along(child.size));
                let delta = match dir {
                    Dir::Right => to[0] - from[0],
                    Dir::Down => to[1] - from[1],
                };
                let ahead = if delta >= 0.0 { 1.0 } else { -1.0 };
                let start = add(from, dir.point(ahead * from_along / 2.0, 0.0));
                let end = add(to, dir.point(-ahead * to_along / 2.0, 0.0));
                let reach = (delta.abs() - from_along / 2.0 - to_along / 2.0).max(0.0);
                let handle = reach * COLUMN_HANDLE;
                let (one, two) = (
                    add(start, dir.point(ahead * handle, 0.0)),
                    add(end, dir.point(-ahead * handle, 0.0)),
                );
                (1..COLUMN_SAMPLES)
                    .map(|step| cubic(start, one, two, end, step as f32 / COLUMN_SAMPLES as f32))
                    .collect()
            }
            // An elbow reads as a route when the two sit mostly above one another; at an angle it is a
            // detour, and the curve of a straight line is what says the two are joined.
            Route::ByAxis => {
                let (dx, dy) = ((to[0] - from[0]).abs(), (to[1] - from[1]).abs());
                if dy >= dx * ELBOW_RATIO {
                    vec![[from[0], down], [to[0], down]]
                } else if dx >= dy * ELBOW_RATIO {
                    vec![[across, from[1]], [across, to[1]]]
                } else {
                    Vec::new()
                }
            }
        };
        bends.into_iter().filter(|point| real(*point)).collect()
    }
}

/// Move a laid-out map so it takes the place its selection held: a ring is centred on the selection, and
/// a decomposition or a cause-and-effect map starts at the selection's left edge, growing leftward
/// instead when the panel would cover it. `panel_left` is the canvas x the panel's card begins at.
pub fn place_at(
    board: Board,
    tree: &MindNode,
    kind: MapKind,
    frame: Rect,
    panel_left: f32,
) -> Board {
    let Some(bounds) = board_box(&board) else {
        return board;
    };
    let (dx, dy) = match fit_kind(kind, tree) {
        MapKind::Radial | MapKind::Compare => (
            frame.centre()[0] - bounds.centre()[0],
            frame.centre()[1] - bounds.centre()[1],
        ),
        MapKind::Tree(Dir::Down) => (frame.centre()[0] - bounds.centre()[0], frame.y - bounds.y),
        MapKind::Tree(Dir::Right) | MapKind::Fishbone => {
            // The words are gone by the time the map is drawn, so the map starts where they stood. A map
            // too wide to run to the right of them grows leftward instead of off the screen, and one that
            // fits nowhere stays at the place and is panned to.
            let start = if frame.x + bounds.w <= panel_left {
                frame.x
            } else if frame.x - bounds.w >= 24.0 {
                frame.x - bounds.w
            } else {
                frame.x
            };
            (start - bounds.x, frame.centre()[1] - bounds.centre()[1])
        }
    };
    board.shifted(dx, dy)
}

/// How far apart two maps are kept, in canvas units.
pub const MAP_GAP: f32 = 32.0;
/// The canvas x a map keeps clear of the screen's left edge, so an answer is never pushed out of sight.
pub const MAP_MARGIN: f32 = 24.0;
/// What one crossed pair of edges costs, in canvas units of width: about a node.
pub const CROSSING_COST: f32 = 180.0;
/// What covering a unit of the board's width in something already drawn costs. More than being wide, so
/// a narrow arrangement that fits beats a wide one that does not.
pub const OVERLAP_COST: f32 = 2.0;
/// A map this small is a ring's shape even when it is deep: few enough nodes to be read at a glance.
const FEW_NODES: usize = 6;

/// What an arrangement costs, all three terms in canvas units so they add up: how wide it comes out, the
/// edges that cross, and the board's own content it would still cover once it has found its place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost {
    pub width: f32,
    pub crossings: usize,
    /// Overlapped area divided by the map's height, which turns it into a width.
    pub covered: f32,
}

impl Cost {
    pub fn score(&self) -> f32 {
        self.width + self.crossings as f32 * CROSSING_COST + self.covered * OVERLAP_COST
    }
}

/// The arrangement the rules picked, with what it cost and where it stands.
#[derive(Debug, Clone)]
pub struct Choice {
    pub board: Board,
    pub kind: MapKind,
    pub cost: Cost,
    pub place: Option<[f32; 2]>,
}

/// The arrangements to lay out for this tree: the one the answer named, or — when it named none — what the
/// shape of the tree allows, best guess first. A hierarchy three levels deep or more is a decomposition and
/// reads to the right, and so is anything big; a shallow or small map goes on a ring; exactly two
/// first-level nodes may be a comparison; a fishbone is only ever asked for, because "these are the causes
/// of that" is not something indentation says.
pub fn candidate_kinds(preference: Option<MapKind>, tree: &MindNode) -> Vec<MapKind> {
    let (depth, nodes) = (levels(tree), count_nodes(tree));
    let mut kinds: Vec<MapKind> = Vec::new();
    let mut add = |kind: MapKind| {
        let fitted = fit_kind(kind, tree);
        if !kinds.contains(&fitted) {
            kinds.push(fitted);
        }
    };
    // What the answer names is an instruction, not a guess: a comparison's shared attribute and a
    // fishbone's spine carry meaning indentation does not, and a decomposition's rightward growth is what
    // makes it read as one. The cost below decides the *place*, and only chooses the family when the
    // answer named none.
    if let Some(kind) = preference {
        add(kind);
        return kinds;
    }
    add(inferred_kind(tree));
    // A small, deep map can also be a ring: fewer nodes than a hand can count at a glance read the same
    // either way, so the cheaper of the two is drawn.
    if depth <= 2 || nodes <= FEW_NODES {
        add(MapKind::Radial);
    }
    if depth >= 3 {
        add(MapKind::Tree(Dir::Right));
    }
    // Exactly two first-level nodes may be a comparison; the layout falls back to the ring by itself when
    // the two have nothing to compare.
    if tree.children.len() == 2 {
        add(MapKind::Compare);
    }
    kinds
}

/// Lay the map out in every arrangement the rules allow, put each one where it would stand, and keep the
/// cheapest. The geometry is the same throughout — the model names a shape of answer, never a coordinate.
#[allow(clippy::too_many_arguments)]
pub fn choose_layout(
    preference: Option<MapKind>,
    tree: &MindNode,
    anchor: [f32; 2],
    room: f32,
    color: &str,
    seed: Option<Rect>,
    taken: &[Rect],
    limit: f32,
    preferred: Option<[f32; 2]>,
    measure: &mut TextMeasure<'_>,
) -> Choice {
    let mut best: Option<(f32, Choice)> = None;
    for kind in candidate_kinds(preference, tree) {
        let board = layout_mindmap(kind, tree, anchor, room, color, measure);
        let board = match seed {
            Some(frame) => place_at(board, tree, kind, frame, limit),
            None => board,
        };
        let (board, place) = free_place(board, taken, limit, preferred);
        let Some(box_) = board_box(&board) else {
            continue;
        };
        let cost = Cost {
            width: box_.w,
            crossings: crossings_of(&board),
            covered: overlap_area(box_, taken) / box_.h.max(1.0),
        };
        let score = cost.score();
        if best.as_ref().is_none_or(|(best, _)| score < *best) {
            best = Some((
                score,
                Choice {
                    board,
                    kind,
                    cost,
                    place,
                },
            ));
        }
    }
    if let Some((_, choice)) = best {
        return choice;
    }
    // A tree with nothing in it: no arrangement has a box to cost, and an empty one is still an answer.
    let kind = fit_kind(preference.unwrap_or_else(|| inferred_kind(tree)), tree);
    let board = layout_mindmap(kind, tree, anchor, room, color, measure);
    Choice {
        board,
        kind,
        cost: Cost {
            width: 0.0,
            crossings: 0,
            covered: 0.0,
        },
        place: None,
    }
}

/// How many nodes a tree holds.
fn count_nodes(node: &MindNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}

/// The edges that cross each other. Two edges that share a node meet there by design; any other pair that
/// meets anywhere is a crossing the reader has to untangle.
fn crossings_of(board: &Board) -> usize {
    let lines: Vec<(&Shape, [u64; 2])> = board
        .shapes
        .iter()
        .filter(|shape| shape.kind == ShapeKind::Line)
        .filter_map(|shape| {
            let from = shape.attach[0]?.shape;
            let to = shape.attach[1]?.shape;
            Some((shape, [from, to]))
        })
        .collect();
    let mut crossings = 0;
    for (index, (one, one_ids)) in lines.iter().enumerate() {
        for (two, two_ids) in &lines[index + 1..] {
            if one_ids.iter().any(|id| two_ids.contains(id)) {
                continue;
            }
            if segments_cross(one.from, one.to, two.from, two.to) {
                crossings += 1;
            }
        }
    }
    crossings
}

/// Whether two segments cross properly: each one has an end on either side of the other.
fn segments_cross(a: [f32; 2], b: [f32; 2], c: [f32; 2], d: [f32; 2]) -> bool {
    let side = |p: [f32; 2], q: [f32; 2], r: [f32; 2]| {
        (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
    };
    let (d1, d2) = (side(a, b, c), side(a, b, d));
    let (d3, d4) = (side(c, d, a), side(c, d, b));
    (d1 > 0.0) != (d2 > 0.0)
        && (d1 != 0.0 && d2 != 0.0)
        && (d3 > 0.0) != (d4 > 0.0)
        && d3 != 0.0
        && d4 != 0.0
}

/// The area of `box_` that `taken` covers.
fn overlap_area(box_: Rect, taken: &[Rect]) -> f32 {
    taken
        .iter()
        .map(|other| {
            let w = (box_.x + box_.w).min(other.x + other.w) - box_.x.max(other.x);
            let h = (box_.y + box_.h).min(other.y + other.h) - box_.y.max(other.y);
            if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
        })
        .sum()
}

/// Where a map of this size can stand among what is already on the board: the place it was asked for when
/// that place is free; else the first free spot walking right, then left, then a row down. `taken` is what
/// the map may not cover — the caller leaves out what this answer is about to replace — and `limit` is the
/// canvas x the panel's card starts at, so a spot that would hide the map under the card is passed over
/// while there is anywhere else to go. A board with no room anywhere keeps the place it asked for.
///
/// Returns the map and the canvas point its own box now starts at, which the caller keeps so the next
/// rebuild of the same answer grows in place instead of hopping to another spot.
pub fn free_place(
    candidate: Board,
    taken: &[Rect],
    limit: f32,
    preferred: Option<[f32; 2]>,
) -> (Board, Option<[f32; 2]>) {
    let Some(box_) = board_box(&candidate) else {
        return (candidate, preferred);
    };
    let clear = |at: Rect| !taken.iter().any(|other| other.intersects(at));
    let at = |x: f32, y: f32| Rect {
        x,
        y,
        w: box_.w,
        h: box_.h,
    };
    // A map that is already on its corner stays there while it fits: it is the same answer growing, not
    // a new one looking for room.
    if let Some([x, y]) = preferred
        && clear(at(x, y))
    {
        return (candidate.shifted(x - box_.x, y - box_.y), preferred);
    }
    // The place it asked for, when nothing is in it — a wide map is still allowed to run on under the
    // card, because a strip cannot hold it and the canvas pans. It may not start off the left edge,
    // though: a map wider than the room is nudged just inside the margin, never half off-screen.
    if clear(box_) {
        let x = box_.x.max(MAP_MARGIN);
        if x == box_.x || clear(at(x, box_.y)) {
            return (candidate.shifted(x - box_.x, 0.0), Some([x, box_.y]));
        }
    }
    let (step_x, step_y) = (box_.w + MAP_GAP, box_.h + MAP_GAP);
    // Candidates in the order a person would look: the nearest first, a row down before a column out, and
    // visible spots before anything else. A map pushed off the screen is worse than one that runs on under
    // the card, and both are worse than covering another map.
    let mut spots: Vec<(u32, u32, f32, f32)> = Vec::new();
    for row in 0..=12 {
        for col in 0..=12 {
            for sign in [1.0_f32, -1.0] {
                if col == 0 && sign < 0.0 {
                    continue;
                }
                spots.push((row, col, sign * col as f32 * step_x, row as f32 * step_y));
            }
        }
    }
    spots.sort_by_key(|(row, col, _, _)| ((*row).max(*col), *col, *row));
    let search = |left: f32, right: f32| {
        spots.iter().find_map(|(_, _, dx, dy)| {
            let spot = at(box_.x + dx, box_.y + dy);
            (spot.x >= left && spot.x + spot.w <= right && clear(spot)).then_some([spot.x, spot.y])
        })
    };
    let found = search(MAP_MARGIN, limit)
        .or_else(|| search(MAP_MARGIN, f32::INFINITY))
        .or_else(|| search(f32::NEG_INFINITY, f32::INFINITY));
    match found {
        Some([x, y]) => (candidate.shifted(x - box_.x, y - box_.y), Some([x, y])),
        None => (candidate, None),
    }
}

/// The box a board's content covers, which is what a map has to be moved by.
fn board_box(board: &Board) -> Option<Rect> {
    board
        .shapes
        .iter()
        .map(Shape::bounds)
        .chain(board.texts.iter().map(TextItem::paint_bounds))
        .reduce(Rect::union)
}

/// Where a diagram starts inside the room the panel leaves: a ring grows from the middle of the room,
/// while a decomposition or a cause-and-effect map runs rightward from its left edge and is allowed to
/// run on under the panel, because a wide tree cannot be made to fit a strip and the canvas pans.
fn origin(kind: MapKind, anchor: [f32; 2], room: f32) -> [f32; 2] {
    match kind {
        MapKind::Radial | MapKind::Compare | MapKind::Tree(Dir::Down) => {
            [anchor[0] + room / 2.0, anchor[1]]
        }
        MapKind::Tree(Dir::Right) | MapKind::Fishbone => [anchor[0] + 24.0, anchor[1]],
    }
}

/// The board for a set of placed nodes: the links first, so the nodes are drawn over them, then each
/// box with its label centred on it.
fn assemble_routed(
    arrangement: &Arrangement,
    color: &str,
    measure: &mut TextMeasure<'_>,
    route: Route,
) -> Board {
    let mut board = Board::default();
    let placed = &arrangement.nodes;
    let mut next = placed.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    // The group boxes go down first: an edge belongs over its branch's boundary, and a node over both.
    for group in &arrangement.groups {
        push_group(&mut board, next, *group, color);
        next += 1;
    }
    for node in placed {
        for parent_id in &node.parents {
            let Some(parent) = placed.iter().find(|other| other.id == *parent_id) else {
                continue;
            };
            let bends = route.bends(parent, node);
            let mut points = Vec::with_capacity(bends.len() + 2);
            points.push(parent.centre);
            points.extend(bends);
            points.push(node.centre);
            push_edge(&mut board, next, &points, *parent_id, node.id, color);
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

/// A group's boundary: a thin rectangle behind its branch, in the ink the map is drawn in but faint, so it
/// reads as a boundary rather than as another node.
fn push_group(board: &mut Board, id: u64, rect: Rect, color: &str) {
    board.shapes.push(Shape {
        kind: ShapeKind::Rect,
        color: faded(color),
        width: GROUP_STROKE,
        from: [rect.x, rect.y],
        to: [rect.x + rect.w, rect.y + rect.h],
        rotate: 0.0,
        id,
        points: Vec::new(),
        roundness: None,
        attach: [None, None],
        fill: None,
    });
}

/// `#rrggbb` at `alpha` of its strength; a colour that already carries an alpha keeps its own hue.
fn faded(color: &str) -> String {
    let hex = color.trim_start_matches('#');
    let rgb = &hex[..hex.len().min(6)];
    format!("#{rgb}{GROUP_ALPHA:02x}")
}

/// One connector, through its bend points. The first and last segments are what the attachments aim
/// along, so an edge that leaves a node sideways stays on its outline however many bends follow.
fn push_edge(
    board: &mut Board,
    id: u64,
    points: &[[f32; 2]],
    from_id: u64,
    to_id: u64,
    color: &str,
) {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return;
    };
    let leave = match points.get(1) {
        Some(bend) => unit(sub(*bend, *first)),
        None => unit(sub(*last, *first)),
    };
    let arrive = match points.len().checked_sub(2).and_then(|i| points.get(i)) {
        Some(bend) => unit(sub(*bend, *last)),
        None => unit(sub(*first, *last)),
    };
    board.shapes.push(Shape {
        kind: ShapeKind::Line,
        color: color.to_string(),
        width: EDGE_STROKE,
        from: *first,
        to: *last,
        rotate: 0.0,
        id,
        points: points[1..points.len() - 1].to_vec(),
        roundness: Some(EDGE_ROUNDNESS),
        attach: [
            Some(Attachment {
                shape: from_id,
                at: leave,
            }),
            Some(Attachment {
                shape: to_id,
                at: arrive,
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
///
/// A first-level branch that has children becomes a group: its descendants are boxed, and the branch's
/// own node stands inside the box's top edge as the title. The band that title stands in is reserved
/// while the branches are spaced, so a box never grows into the branch above it.
fn tree_place(
    tree: &MindNode,
    anchor: [f32; 2],
    measure: &mut TextMeasure<'_>,
    dir: Dir,
) -> Arrangement {
    let depth = levels(tree);
    let mut layer = vec![0.0f32; depth];
    measure_depths(tree, 0, &mut layer, measure, dir);
    // Each layer starts where the deepest node of the layer before it ended, along the depth axis.
    let (along_start, across_centre) = match dir {
        Dir::Right => (anchor[0], anchor[1]),
        Dir::Down => (anchor[1], anchor[0]),
    };
    let mut layers = Vec::with_capacity(depth);
    let mut at = along_start;
    for extent in &layer {
        layers.push(at);
        at += extent + TREE_COLUMN_GAP;
    }
    let bands: Vec<f32> = tree
        .children
        .iter()
        .map(|child| title_band(child, measure))
        .collect();
    // What each branch takes, room for its box included, so the map is centred on what it really covers.
    let spacing: Vec<f32> = tree
        .children
        .iter()
        .zip(&bands)
        .map(|(child, band)| {
            subtree_across(child, measure, dir)
                + band
                + TREE_ROW_GAP
                + if child.children.is_empty() {
                    0.0
                } else {
                    GROUP_PADDING
                }
        })
        .collect();
    let total = spacing.iter().sum::<f32>() - TREE_ROW_GAP;
    let mut placed = Vec::new();
    let mut groups = Vec::new();
    let mut next = 1u64;
    // The root first, level with the middle of what hangs from it.
    let root_size = node_size(&tree.text, measure);
    placed.push(Placed {
        id: next,
        parents: Vec::new(),
        centre: dir.point(layers[0] + dir.along(root_size) / 2.0, across_centre),
        size: root_size,
        text: tree.text.clone(),
    });
    next += 1;
    let root_id = 1;
    let mut cursor = across_centre - total / 2.0;
    for (index, child) in tree.children.iter().enumerate() {
        let first = next;
        let branch_id = place_tree(
            child,
            1,
            cursor + bands[index],
            &layers,
            &mut next,
            measure,
            &mut placed,
            Some(root_id),
            dir,
        );
        // A branch with nothing under it is a leaf, and a box around nothing reads as noise.
        if !child.children.is_empty()
            && let Some(descent) = union_of(&placed, first + 1, next - 1)
            && let Some(box_) = box_branch(&mut placed, branch_id, descent, dir)
        {
            groups.push(box_);
        }
        cursor += spacing[index];
    }
    Arrangement {
        nodes: placed,
        groups,
    }
}

/// The room a branch's title keeps above its descendants: the title itself, the gaps around it and the
/// box's own padding. None for a leaf, which gets no box, so the branches sit as tightly as before.
fn title_band(node: &MindNode, measure: &mut TextMeasure<'_>) -> f32 {
    if node.children.is_empty() {
        return 0.0;
    }
    node_size(&node.text, measure)[1] + 2.0 * GROUP_TITLE_GAP + GROUP_PADDING
}

/// The box around the nodes with ids `first..=last`, which placement numbers contiguously.
fn union_of(nodes: &[Placed], first: u64, last: u64) -> Option<Rect> {
    (first..=last)
        .filter_map(|id| nodes.get(id as usize - 1))
        .map(|node| Rect {
            x: node.centre[0] - node.size[0] / 2.0,
            y: node.centre[1] - node.size[1] / 2.0,
            w: node.size[0],
            h: node.size[1],
        })
        .reduce(Rect::union)
}

/// Box a branch's descendants and stand the branch itself inside the box's top edge as the title.
///
/// The title keeps its own column — the box reaches left to include it — so the connector that arrives
/// from the parent still crosses sideways and the edge keeps its bend. Only the title's height moves: it
/// leaves the middle of its band and takes the band above it.
fn box_branch(nodes: &mut [Placed], branch_id: u64, descent: Rect, dir: Dir) -> Option<Rect> {
    let branch = nodes.iter_mut().find(|node| node.id == branch_id)?;
    let held = Rect {
        x: descent.x - GROUP_PADDING,
        y: descent.y - GROUP_PADDING,
        w: descent.w + 2.0 * GROUP_PADDING,
        h: descent.h + 2.0 * GROUP_PADDING,
    };
    // The box reaches the title's own edge rather than padding past it, so the corridor the parent's
    // edge runs down stays clear of the boundary it runs beside.
    let left = held.x.min(branch.centre[0] - branch.size[0] / 2.0);
    let right = held
        .x
        .max(branch.centre[0] + branch.size[0] / 2.0)
        .max(held.x + held.w)
        + GROUP_PADDING;
    let top = held.y - (branch.size[1] + 2.0 * GROUP_TITLE_GAP);
    let box_ = Rect {
        x: left,
        y: top,
        w: right - left,
        h: held.y + held.h - top,
    };
    // A column of children is headed by its title at the edge the parent arrives on; a row of children
    // is headed by it centred over them, which is how an org chart reads.
    if dir == Dir::Down {
        branch.centre[0] = box_.x + box_.w / 2.0;
    }
    branch.centre[1] = box_.y + GROUP_TITLE_GAP + branch.size[1] / 2.0;
    Some(box_)
}

fn levels(node: &MindNode) -> usize {
    1 + node.children.iter().map(levels).max().unwrap_or(0)
}

fn measure_depths(
    node: &MindNode,
    depth: usize,
    layer: &mut [f32],
    measure: &mut TextMeasure<'_>,
    dir: Dir,
) {
    let along = dir.along(node_size(&node.text, measure));
    layer[depth] = layer[depth].max(along);
    for child in &node.children {
        measure_depths(child, depth + 1, layer, measure, dir);
    }
}

/// The vertical room a subtree needs: its own box, or its children's bands and the gaps between them.
fn subtree_across(node: &MindNode, measure: &mut TextMeasure<'_>, dir: Dir) -> f32 {
    let own = dir.across(node_size(&node.text, measure));
    if node.children.is_empty() {
        return own;
    }
    let children: f32 = node
        .children
        .iter()
        .map(|child| subtree_across(child, measure, dir))
        .sum::<f32>()
        + TREE_ROW_GAP * (node.children.len() - 1) as f32;
    own.max(children)
}

#[allow(clippy::too_many_arguments)]
fn place_tree(
    node: &MindNode,
    depth: usize,
    start: f32,
    layers: &[f32],
    next: &mut u64,
    measure: &mut TextMeasure<'_>,
    out: &mut Vec<Placed>,
    parent: Option<u64>,
    dir: Dir,
) -> u64 {
    let id = *next;
    *next += 1;
    let band = subtree_across(node, measure, dir);
    let size = node_size(&node.text, measure);
    out.push(Placed {
        id,
        parents: parent.into_iter().collect(),
        centre: dir.point(layers[depth] + dir.along(size) / 2.0, start + band / 2.0),
        size,
        text: node.text.clone(),
    });
    if node.children.is_empty() {
        return id;
    }
    let children: f32 = node
        .children
        .iter()
        .map(|child| subtree_across(child, measure, dir))
        .sum::<f32>()
        + TREE_ROW_GAP * (node.children.len() - 1) as f32;
    let mut at = start + (band - children) / 2.0;
    for child in &node.children {
        place_tree(
            child,
            depth + 1,
            at,
            layers,
            next,
            measure,
            out,
            Some(id),
            dir,
        );
        at += subtree_across(child, measure, dir) + TREE_ROW_GAP;
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
        // The spine runs straight; only the ribs and their children are curves' business.
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
                &[cause_centre, centre],
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
) -> Option<Arrangement> {
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
    Some(Arrangement {
        nodes: placed,
        // A comparison already reads as two columns with a shared one between them: a boundary around
        // each side would be an empty frame, so only a decomposition's branches are boxed.
        groups: Vec::new(),
    })
}

/// How much of the map hangs below a node; a leaf owns one slice.
fn leaves(node: &MindNode) -> u32 {
    if node.children.is_empty() {
        1
    } else {
        node.children.iter().map(leaves).sum()
    }
}

fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
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
        let body = request_body(&settings, &messages, "second", None);
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

    #[test]
    fn the_rules_offer_only_arrangements_a_tree_can_take() {
        let two = parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n    Wheels\n").unwrap();
        assert_eq!(
            candidate_kinds(None, &two),
            vec![MapKind::Radial, MapKind::Compare]
        );
        // Three levels: a decomposition is allowed, and a small one may still be a ring.
        let deep = parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n      Camera\n").unwrap();
        assert_eq!(
            candidate_kinds(None, &deep),
            vec![MapKind::Tree(Dir::Right), MapKind::Radial]
        );
        // Three levels and more than a handful of nodes: only the decomposition reads.
        let wide = parse_mindmap(
            "mindmap\n  root((Robot))\n    A\n      A1\n    B\n      B1\n    C\n      C1\n",
        )
        .unwrap();
        assert_eq!(
            candidate_kinds(None, &wide),
            vec![MapKind::Tree(Dir::Right)]
        );
        // What the answer names is what is drawn, fitted when the tree cannot take it at all.
        assert_eq!(
            candidate_kinds(Some(MapKind::Compare), &two),
            vec![MapKind::Compare]
        );
        assert_eq!(
            candidate_kinds(Some(MapKind::Compare), &deep),
            vec![MapKind::Radial]
        );
        assert_eq!(
            candidate_kinds(Some(MapKind::Fishbone), &two),
            vec![MapKind::Fishbone]
        );
        assert_eq!(
            candidate_kinds(Some(MapKind::Tree(Dir::Right)), &two),
            vec![MapKind::Tree(Dir::Right)]
        );
        assert_eq!(
            candidate_kinds(Some(MapKind::Radial), &wide),
            vec![MapKind::Radial]
        );
    }

    #[test]
    fn a_crossed_pair_of_edges_is_counted_and_a_shared_node_is_not() {
        let edge = |id: u64, a: u64, b: u64, from: [f32; 2], to: [f32; 2]| Shape {
            kind: ShapeKind::Line,
            color: "#ffffff".into(),
            width: 1.5,
            from,
            to,
            rotate: 0.0,
            id,
            points: Vec::new(),
            roundness: None,
            attach: [
                Some(Attachment {
                    shape: a,
                    at: [0.0, 0.0],
                }),
                Some(Attachment {
                    shape: b,
                    at: [0.0, 0.0],
                }),
            ],
            fill: None,
        };
        let mut board = Board::default();
        // Two edges that cross in the middle.
        board.shapes.push(edge(1, 1, 2, [0.0, 0.0], [100.0, 100.0]));
        board.shapes.push(edge(2, 3, 4, [0.0, 100.0], [100.0, 0.0]));
        assert_eq!(crossings_of(&board), 1);
        // Two that share a node meet there by design.
        let mut shared = Board::default();
        shared
            .shapes
            .push(edge(1, 1, 2, [0.0, 0.0], [100.0, 100.0]));
        shared
            .shapes
            .push(edge(2, 2, 3, [100.0, 100.0], [100.0, 0.0]));
        assert_eq!(crossings_of(&shared), 0);
        // Two that never meet.
        let mut apart = Board::default();
        apart.shapes.push(edge(1, 1, 2, [0.0, 0.0], [100.0, 0.0]));
        apart.shapes.push(edge(2, 3, 4, [0.0, 40.0], [100.0, 40.0]));
        assert_eq!(crossings_of(&apart), 0);
    }

    #[test]
    fn the_cheapest_arrangement_is_the_one_drawn() {
        let tree = parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n      Camera\n").unwrap();
        let mut measure = |_: &str, size: f32, _: Option<f32>| (size, size);
        let anchor = [48.0, 300.0];
        let limit = 400.0;
        let chosen = choose_layout(
            None,
            &tree,
            anchor,
            344.0,
            "#ffffff",
            None,
            &[],
            limit,
            None,
            &mut measure,
        );
        // Every arrangement the rules allow is costed the same way, and none is cheaper than the winner.
        let mut widths = Vec::new();
        for kind in candidate_kinds(None, &tree) {
            let board = layout_mindmap(kind, &tree, anchor, 344.0, "#ffffff", &mut measure);
            let (board, _) = free_place(board, &[], limit, None);
            widths.push(board_box(&board).unwrap().w);
        }
        assert!(widths.contains(&chosen.cost.width), "{widths:?} {chosen:?}");
        assert!(
            chosen.cost.width <= widths.iter().copied().fold(f32::INFINITY, f32::min) + 0.5,
            "a wider arrangement won: {widths:?} {chosen:?}"
        );
    }

    #[test]
    fn an_arrangement_that_would_cover_the_board_gives_way() {
        let tree = parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n      Camera\n").unwrap();
        let mut measure = |_: &str, size: f32, _: Option<f32>| (size, size);
        // Everything the ring would cover is already used, so the other arrangement, or another place,
        // has to win.
        let blocked = [Rect {
            x: 0.0,
            y: 0.0,
            w: 900.0,
            h: 600.0,
        }];
        let chosen = choose_layout(
            None,
            &tree,
            [48.0, 300.0],
            344.0,
            "#ffffff",
            None,
            &blocked,
            400.0,
            None,
            &mut measure,
        );
        assert!(chosen.cost.covered <= 1.0, "{chosen:?}");
        assert!(chosen.place.is_some(), "it found no place at all");
    }

    /// A boundary is the map's ink with the group's alpha, whatever the colour was.
    fn is_group_ink(color: &str) -> bool {
        color.len() == 9 && color.ends_with(&format!("{GROUP_ALPHA:02x}"))
    }

    #[test]
    fn a_branch_is_boxed_with_its_title_inside_the_top_edge() {
        let tree = parse_mindmap(
            "mindmap\n  root((Robot))\n    Sensors\n      Camera\n      Lidar\n    Actuators\n      Wheels\n",
        )
        .unwrap();
        let mut measure = |_: &str, size: f32, _: Option<f32>| (size, size);
        let board = layout_mindmap(
            MapKind::Tree(Dir::Right),
            &tree,
            [0.0, 300.0],
            600.0,
            "#e01b24",
            &mut measure,
        );
        let boxes: Vec<Rect> = board
            .shapes
            .iter()
            .filter(|shape| shape.kind == ShapeKind::Rect && is_group_ink(&shape.color))
            .map(Shape::bounds)
            .collect();
        // Two branches with children, two boxes; the root's own children are not boxed separately.
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        let nodes: Vec<Rect> = board
            .shapes
            .iter()
            .filter(|shape| shape.color == "#e01b24" && shape.kind == ShapeKind::Rect)
            .map(Shape::bounds)
            .collect();
        assert_eq!(
            nodes.len(),
            6,
            "six nodes: root, two branches, three leaves"
        );
        for box_ in &boxes {
            // The title is a node standing in the box's top band, and its descendants are below it.
            // Painted bounds against a painted box: the title is flush with the boundary, so its own
            // stroke may stand a fraction outside it.
            let titled = nodes.iter().any(|node| {
                node.y >= box_.y
                    && node.y + node.h / 2.0 <= box_.y + 0.4 * box_.h
                    && node.x >= box_.x - 0.5
                    && node.x + node.w <= box_.x + box_.w + 0.5
            });
            assert!(titled, "no title inside the top edge of {box_:?}");
            let held = nodes
                .iter()
                .filter(|node| {
                    node.x >= box_.x - 0.5
                        && node.x + node.w <= box_.x + box_.w + 0.5
                        && node.y >= box_.y
                })
                .count();
            assert!(held >= 2, "{box_:?} holds {held} of {nodes:?}");
            // Branches never overlap: each box keeps to its own band.
            for other in &boxes {
                if *other != *box_ {
                    assert!(
                        box_.y + box_.h <= other.y + 0.5 || other.y + other.h <= box_.y + 0.5,
                        "{box_:?} runs into {other:?}"
                    );
                }
            }
        }
        // Every edge is drawn as a curve, and a decomposition's arrive and leave sideways.
        let edges: Vec<&Shape> = board
            .shapes
            .iter()
            .filter(|shape| shape.kind == ShapeKind::Line)
            .collect();
        assert_eq!(edges.len(), 5, "one edge per parent-child pair");
        for edge in &edges {
            assert_eq!(edge.roundness, Some(2), "an edge is drawn curved");
            // The S is drawn through sampled points, so an edge that crosses columns carries several:
            // it is a curve of its own rather than a chord shared with its siblings.
            assert!(
                edge.points.len() >= 2,
                "a tree edge has a shape of its own: {:?}",
                edge.points
            );
            let at = edge.attach[0].expect("an edge leaves its parent").at;
            assert!(at[0].abs() > at[1].abs(), "it leaves sideways: {at:?}");
        }
    }

    #[test]
    fn a_ring_keeps_its_spokes_straight() {
        let tree = parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n    Wheels\n").unwrap();
        let mut measure = |_: &str, size: f32, _: Option<f32>| (size, size);
        let board = layout_mindmap(
            MapKind::Radial,
            &tree,
            [0.0, 0.0],
            600.0,
            "#e01b24",
            &mut measure,
        );
        let edges: Vec<&Shape> = board
            .shapes
            .iter()
            .filter(|shape| shape.kind == ShapeKind::Line)
            .collect();
        assert_eq!(edges.len(), 2);
        for edge in edges {
            assert_eq!(edge.roundness, Some(2));
            assert!(
                edge.points.is_empty(),
                "a spoke needs no bend: {:?}",
                edge.points
            );
        }
        assert!(
            !board.shapes.iter().any(|shape| is_group_ink(&shape.color)),
            "a ring has no groups"
        );
    }

    #[test]
    fn a_map_stands_clear_of_the_ones_before_it() {
        let tree = parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n    Actuators\n").unwrap();
        let mut measure = |_: &str, size: f32, _: Option<f32>| (size, size);
        let mut map = || {
            layout_mindmap(
                MapKind::Radial,
                &tree,
                [0.0, 0.0],
                600.0,
                "#ffffff",
                &mut measure,
            )
        };
        // Nothing in the way: the map keeps the place it asked for, and says where that is.
        let first = map();
        let box_ = board_box(&first).unwrap();
        let (placed, at) = free_place(first, &[], 1200.0, None);
        assert_eq!(board_box(&placed), Some(box_));
        assert_eq!(at, Some([box_.x, box_.y]));
        // Something already stands there: the map walks out of it, and says where it landed.
        let taken = [box_.grown(4.0)];
        let (placed, at) = free_place(map(), &taken, 1200.0, None);
        let moved = board_box(&placed).unwrap();
        assert!(!moved.intersects(box_.grown(4.0)), "{moved:?} on {box_:?}");
        assert_eq!(at, Some([moved.x, moved.y]));
        // It steps aside by a gap, and stays on the screen: a row down is the first place it looks.
        let apart_x = moved.x >= box_.x + box_.w + MAP_GAP || moved.x + moved.w + MAP_GAP <= box_.x;
        let apart_y = moved.y >= box_.y + box_.h + MAP_GAP || moved.y + moved.h + MAP_GAP <= box_.y;
        assert!(
            apart_x || apart_y,
            "{moved:?} is not a gap away from {box_:?}"
        );
        assert!(moved.x >= MAP_MARGIN, "{moved:?} was pushed off the screen");
        // The corner an answer already stands on is kept while it still fits, so a streaming map grows
        // in place instead of hopping.
        let (placed, at) = free_place(map(), &taken, 1200.0, Some([moved.x, moved.y]));
        assert_eq!(board_box(&placed), Some(moved));
        assert_eq!(at, Some([moved.x, moved.y]));
    }

    #[test]
    fn a_map_that_would_hide_under_the_panel_steps_aside() {
        let tree = parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n").unwrap();
        let mut measure = |_: &str, size: f32, _: Option<f32>| (size, size);
        let map = layout_mindmap(
            MapKind::Radial,
            &tree,
            [600.0, 300.0],
            600.0,
            "#ffffff",
            &mut measure,
        );
        let box_ = board_box(&map).unwrap();
        // Something stands where it asked to be, and the card begins before the map ends: it steps aside
        // to a free spot that is not under the card, rather than walking right into the panel.
        let limit = box_.x + 20.0;
        let taken = [Rect {
            x: box_.x,
            y: box_.y,
            w: 10.0,
            h: 10.0,
        }];
        let (placed, _) = free_place(map, &taken, limit, None);
        let moved = board_box(&placed).unwrap();
        assert!(
            !moved.intersects(taken[0]),
            "{moved:?} covers what stood there"
        );
        assert!(
            moved.x + moved.w <= limit,
            "{moved:?} still under the card at {limit}"
        );
    }

    #[test]
    fn a_board_with_no_room_keeps_the_place_it_asked_for() {
        let tree = parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n").unwrap();
        let mut measure = |_: &str, size: f32, _: Option<f32>| (size, size);
        let map = layout_mindmap(
            MapKind::Radial,
            &tree,
            [0.0, 0.0],
            600.0,
            "#ffffff",
            &mut measure,
        );
        let box_ = board_box(&map).unwrap();
        let everywhere = [Rect {
            x: box_.x - 100_000.0,
            y: box_.y - 100_000.0,
            w: 200_000.0,
            h: 200_000.0,
        }];
        let (placed, at) = free_place(map, &everywhere, 1200.0, None);
        assert_eq!(
            board_box(&placed),
            Some(box_),
            "a crowded board is not a lost board"
        );
        assert_eq!(at, None);
    }

    #[test]
    fn a_request_to_expand_a_selection_names_it() {
        // The system turn is the only place the model hears what was selected, and it stays the plain
        // rules when nothing was.
        assert_eq!(system_prompt(None), SYSTEM_PROMPT);
        assert_eq!(system_prompt(Some("   ")), SYSTEM_PROMPT);
        let named = system_prompt(Some("Robot / Sensors"));
        assert!(named.starts_with(SYSTEM_PROMPT), "{named}");
        assert!(named.contains("Robot / Sensors"), "{named}");
    }

    #[test]
    fn a_map_takes_the_place_of_the_selection_it_expands() {
        // Two levels stay a ring, three become a decomposition, which is the rule the layout applies.
        let ring_tree =
            parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n    Actuators\n").unwrap();
        let tree = parse_mindmap("mindmap\n  root((Robot))\n    Sensors\n      Camera\n").unwrap();
        let mut measure = |_: &str, size: f32, _: Option<f32>| (size, size);
        let frame = Rect {
            x: 400.0,
            y: 300.0,
            w: 120.0,
            h: 80.0,
        };
        // A ring is centred on the selection it expands.
        let ring = layout_mindmap(
            MapKind::Radial,
            &ring_tree,
            [0.0, 0.0],
            600.0,
            "#ffffff",
            &mut measure,
        );
        let placed = place_at(ring, &ring_tree, MapKind::Radial, frame, 800.0);
        let box_ = board_box(&placed).unwrap();
        assert!(
            (box_.centre()[0] - frame.centre()[0]).abs() < 0.5,
            "{box_:?}"
        );
        assert!(
            (box_.centre()[1] - frame.centre()[1]).abs() < 0.5,
            "{box_:?}"
        );
        // A tree starts where the words stood: they are gone by the time it is drawn.
        let tree_map = layout_mindmap(
            MapKind::Tree(Dir::Right),
            &tree,
            [0.0, 0.0],
            600.0,
            "#ffffff",
            &mut measure,
        );
        let placed = place_at(tree_map, &tree, MapKind::Tree(Dir::Right), frame, 1200.0);
        let box_ = board_box(&placed).unwrap();
        assert!((box_.x - frame.x).abs() < 0.5, "{box_:?}");
        assert!(
            (box_.centre()[1] - frame.centre()[1]).abs() < 0.5,
            "{box_:?}"
        );
        // A tree too wide to run to the right of them grows leftward, ending where they stood, rather
        // than under the panel.
        let tree_map = layout_mindmap(
            MapKind::Tree(Dir::Right),
            &tree,
            [0.0, 0.0],
            600.0,
            "#ffffff",
            &mut measure,
        );
        let right_frame = Rect {
            x: 600.0,
            y: 300.0,
            w: 120.0,
            h: 80.0,
        };
        let placed = place_at(
            tree_map,
            &tree,
            MapKind::Tree(Dir::Right),
            right_frame,
            700.0,
        );
        let box_ = board_box(&placed).unwrap();
        assert!(
            (box_.x + box_.w - right_frame.x).abs() < 0.5,
            "the map runs under the panel: {box_:?}"
        );
    }
}
