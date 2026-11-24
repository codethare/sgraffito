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

use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
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
The block must start with `mindmap` and use two spaces per indentation level.
Use one root and at most 40 nodes. Do not add any text after the block.";

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

const GO_BASE: &str = "https://opencode.ai/inference/go";
const ZEN_BASE: &str = "https://opencode.ai/inference";

/// The request's shell. Static: the endpoint, the key, the session and the timeout come from the
/// environment, so nothing secret is ever an argv of `bwrap` or of `curl`.
const CURL_SCRIPT: &str = "\
set --
if [ -n \"$SGRAFFITO_AI_KEY\" ]; then
  set -- --header \"Authorization: Bearer $SGRAFFITO_AI_KEY\"
fi
exec curl --silent --show-error --no-buffer --fail-with-body \\
  --connect-timeout 15 --max-time \"$SGRAFFITO_AI_TIMEOUT\" \\
  --request POST \\
  --header \"Content-Type: application/json\" \\
  --header \"Accept: text/event-stream\" \\
  --header \"x-opencode-session: $SGRAFFITO_AI_SESSION\" \\
  \"$@\" --data-binary @- \\
  --write-out '\\n__SGRAFFITO_HTTP__%{response_code}\\n' \\
  --url \"$SGRAFFITO_AI_ENDPOINT\"";

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

/// The base a request is posted to. A base that already names the completions path is used as it is,
/// so a gateway with its own layout is not given a second suffix.
fn endpoint_for(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
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
pub struct Chat {
    pub open: bool,
    /// The output the panel is drawn on. One panel, on one screen.
    pub output: Option<u32>,
    /// Whether the input line owns the keyboard.
    pub focus: bool,
    pub input: TextBuffer,
    pub preedit: String,
    pub messages: Vec<Message>,
    pub error: Option<String>,
    /// How far the log is scrolled, in logical pixels from its top.
    pub scroll: f32,
    /// Whether the log follows the end of the stream. A user who scrolls up stops following.
    pub follow: bool,
    /// The session header the Go endpoint wants. One id per process, so a follow-up continues it.
    pub session: String,
    /// The board the next answer is drawn on, fixed when the prompt is sent.
    pub target_board: String,
    /// Where the diagram's root goes, in canvas coordinates.
    pub anchor: [f32; 2],
    /// The ink the diagram is drawn in.
    pub color: String,
    /// The diagram as it stands, drawn live and committed when the answer completes.
    pub preview: Option<Board>,
    /// The ids of the shapes this chat placed last time, so a follow-up replaces its own diagram.
    pub placed_shapes: Vec<u64>,
    /// When the preview was last rebuilt.
    pub last_preview: Instant,
    /// A press that went down inside the panel owns the pointer until it is released.
    pub pointer_captured: bool,
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
        Self {
            open: false,
            output: None,
            focus: false,
            input: TextBuffer::new(""),
            preedit: String::new(),
            messages: Vec::new(),
            error: None,
            scroll: 0.0,
            follow: true,
            session: session_id(),
            target_board: String::new(),
            anchor: [0.0, 0.0],
            color: String::new(),
            preview: None,
            placed_shapes: Vec::new(),
            last_preview: Instant::now(),
            pointer_captured: false,
            generation: None,
            rx: None,
        }
    }

    /// Whether a request is still arriving.
    pub fn streaming(&self) -> bool {
        self.generation.is_some()
    }

    /// The transcript and input as the renderer draws them. A copy: the window's state is the app's.
    pub fn view(&self) -> AiView {
        AiView {
            messages: self
                .messages
                .iter()
                .map(|message| AiMessageView {
                    user: message.role == Role::User,
                    text: message.text.clone(),
                    streaming: message.streaming,
                })
                .collect(),
            input: self.input.clone(),
            preedit: self.preedit.clone(),
            streaming: self.streaming(),
            error: self.error.clone(),
            scroll: self.scroll,
        }
    }

    /// Send the prompt. The caller has already appended the user's message and an empty assistant one,
    /// so the body is built from the transcript as it stands.
    pub fn start(&mut self, settings: AiSettings, ping: Ping) -> Result<(), String> {
        if self.streaming() {
            return Err("a request is already running".into());
        }
        let Some(prompt) = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .map(|message| message.text.clone())
        else {
            return Err("there is no prompt to send".into());
        };
        let body = request_body(&settings, &self.messages, &prompt).to_string();
        crate::log::write(&format!(
            "ai: {} model {}",
            settings.endpoint, settings.model
        ));
        let mut command = sandbox(&settings, &self.session);
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start bwrap: {e}"))?;
        if let Some(mut stdin) = child.stdin.take()
            && let Err(e) = stdin.write_all(body.as_bytes())
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
        thread::spawn(move || read_stream(stdout, tx, ping));
        self.generation = Some(Generation { child });
        self.rx = Some(rx);
        Ok(())
    }

    /// Drain what the reader has reported. `true` when anything changed on screen.
    pub fn poll(&mut self) -> bool {
        let mut events = Vec::new();
        if let Some(rx) = self.rx.as_ref() {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        } else {
            return false;
        }
        if events.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut finished = false;
        let mut oversized = false;
        for event in events {
            match event {
                AiEvent::Delta(delta) => {
                    let Some(last) = self.messages.last_mut() else {
                        continue;
                    };
                    if last.role != Role::Assistant {
                        continue;
                    }
                    if last.text.len() + delta.len() > MAX_ASSISTANT_BYTES {
                        oversized = true;
                        break;
                    }
                    last.text.push_str(&delta);
                    changed = true;
                }
                AiEvent::Done(_status) => finished = true,
                AiEvent::Error(message) => {
                    self.error = Some(message);
                    changed = true;
                    finished = true;
                }
            }
        }
        if oversized {
            self.error = Some("AI response too large".into());
            changed = true;
            finished = true;
        }
        if finished {
            // Dropping the receiver stops the reader as soon as its next send fails, and dropping the
            // generation kills and reaps the child.
            self.rx = None;
            self.generation = None;
            if let Some(last) = self.messages.last_mut() {
                last.streaming = false;
            }
        }
        changed
    }

    /// Stop the request in flight, keeping what has already arrived. Closing the panel is one caller;
    /// a failed or stopped request is another.
    pub fn abort(&mut self) {
        self.rx = None;
        self.generation = None;
        for message in &mut self.messages {
            message.streaming = false;
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

/// The sandboxed request. Everything the child needs is in its own environment, so the key and the
/// endpoint never appear in `bwrap`'s or `curl`'s argv where any process on the machine could read
/// them. `env_clear` is what makes the sandbox's environment exactly this list: bwrap's own
/// `--clearenv` would drop these too, since they are carried through bwrap.
fn sandbox(settings: &AiSettings, session: &str) -> Command {
    let mut command = Command::new("bwrap");
    command.args([
        "--unshare-all",
        "--share-net",
        "--die-with-parent",
        "--ro-bind",
        "/usr",
        "/usr",
    ]);
    // `/bin`, `/lib` and `/lib64` are symlinks into `/usr` on most current distributions and real
    // directories on the rest. Binding the source of a symlink would put a directory where the shell
    // expects the link, so the link is reproduced as a link and a real directory is bound.
    for path in ["/bin", "/sbin", "/lib", "/lib64"] {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if let Ok(target) = std::fs::read_link(path) {
                    command.args(["--symlink", &target.to_string_lossy(), path]);
                }
            }
            Ok(_) => {
                command.args(["--ro-bind", path, path]);
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
            command.args(["--ro-bind", path, path]);
        }
    }
    command.args([
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--chdir",
        "/tmp",
        "--",
        "/bin/sh",
        "-c",
        CURL_SCRIPT,
    ]);
    command.env_clear();
    command.env("PATH", "/usr/bin:/bin");
    command.env("HOME", "/tmp");
    command.env("SGRAFFITO_AI_ENDPOINT", &settings.endpoint);
    command.env("SGRAFFITO_AI_KEY", &settings.key);
    command.env("SGRAFFITO_AI_SESSION", session);
    command.env(
        "SGRAFFITO_AI_TIMEOUT",
        settings.timeout.as_secs().max(1).to_string(),
    );
    // A proxy is how a network behind one is reached, so it is passed through; nothing else is.
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"] {
        if let Ok(value) = std::env::var(name) {
            command.env(name, value);
        }
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Read the stream on its own thread: `data:` lines carry deltas, the write-out line carries the HTTP
/// status, and the end of the stream decides whether the request worked.
fn read_stream(stdout: impl std::io::Read, tx: mpsc::Sender<AiEvent>, ping: Ping) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut status: Option<u16> = None;
    let mut body = String::new();
    let mut content = false;
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
                report(&tx, &ping, AiEvent::Error(message.to_string()));
                return;
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
    if code >= 400 {
        let body = body.trim();
        report(
            &tx,
            &ping,
            AiEvent::Error(if body.is_empty() {
                format!("HTTP {code}")
            } else {
                format!("HTTP {code}: {body}")
            }),
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

/// A node as it will be drawn: its place on the ring, its box, and the node it hangs from.
struct Planned {
    id: u64,
    parent: Option<u64>,
    centre: [f32; 2],
    size: [f32; 2],
    text: String,
}

/// The diagram's box padding around its text, and the ring step between depths.
const NODE_TEXT_SIZE: f32 = 16.0;
const NODE_PADDING: f32 = 10.0;
const NODE_MIN_WIDTH: f32 = 120.0;
const NODE_MAX_WIDTH: f32 = 240.0;
const NODE_MIN_HEIGHT: f32 = 40.0;
const NODE_STROKE: f32 = 2.0;
const EDGE_STROKE: f32 = 1.5;
const RING_STEP: f32 = 190.0;

/// Lay the map out radially about `anchor`: the root at the centre, each depth on its own ring, and
/// each child given an angular slice proportional to how much of the map hangs below it.
///
/// `measure` is the renderer's own text measurement, so the boxes fit the glyphs rather than an
/// estimate. Every node is a rectangle (the document has no rounded box), every parent-child link is a
/// line attached to both ends, and one text box labels each node.
pub fn layout_mindmap(
    tree: &MindNode,
    anchor: [f32; 2],
    color: &str,
    measure: &mut TextMeasure<'_>,
) -> Board {
    let mut planned: Vec<Planned> = Vec::new();
    let mut next = 1u64;
    place(
        tree,
        0,
        0.0,
        std::f32::consts::TAU,
        anchor,
        &mut next,
        measure,
        &mut planned,
        None,
    );
    let mut board = Board::default();
    // Edges first: the shapes are painted in order, so a line drawn after its nodes would cross them.
    for node in &planned {
        let Some(parent_id) = node.parent else {
            continue;
        };
        let Some(parent) = planned.iter().find(|other| other.id == parent_id) else {
            continue;
        };
        board.shapes.push(Shape {
            kind: ShapeKind::Line,
            color: color.to_string(),
            width: EDGE_STROKE,
            from: parent.centre,
            to: node.centre,
            rotate: 0.0,
            id: next,
            points: Vec::new(),
            roundness: None,
            attach: [
                Some(Attachment {
                    shape: parent_id,
                    at: unit(sub(node.centre, parent.centre)),
                }),
                Some(Attachment {
                    shape: node.id,
                    at: unit(sub(parent.centre, node.centre)),
                }),
            ],
            fill: None,
        });
        next += 1;
    }
    for node in &planned {
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
    // The stored endpoints are the node centres; the attachments are what put the lines on the
    // outlines. This is the same call the document makes when it loads a board.
    board.sync_connectors();
    board
}

/// Place one subtree: `start`/`end` is the angular slice it owns, and `depth` is its ring.
#[allow(clippy::too_many_arguments)]
fn place(
    node: &MindNode,
    depth: usize,
    start: f32,
    end: f32,
    anchor: [f32; 2],
    next: &mut u64,
    measure: &mut TextMeasure<'_>,
    out: &mut Vec<Planned>,
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
    let natural = measure(&node.text, NODE_TEXT_SIZE, None);
    let width = (natural.0 + 2.0 * NODE_PADDING).clamp(NODE_MIN_WIDTH, NODE_MAX_WIDTH);
    let wrap = (width - 2.0 * NODE_PADDING).max(1.0);
    let wrapped = measure(&node.text, NODE_TEXT_SIZE, Some(wrap));
    let height = (wrapped.1 + 2.0 * NODE_PADDING).max(NODE_MIN_HEIGHT);
    out.push(Planned {
        id,
        parent,
        centre,
        size: [width, height],
        text: node.text.clone(),
    });
    let total: u32 = node.children.iter().map(leaves).sum();
    let mut from = start;
    for child in &node.children {
        let share = (end - start) * leaves(child) as f32 / total as f32;
        place(
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

    /// The one property a sandboxed request has to keep: the key goes to the child's environment, never
    /// to an argv of `bwrap` or `curl`, where any process on the machine could read it.
    #[test]
    fn the_key_is_an_environment_value_and_never_an_argument() {
        let settings = AiSettings {
            endpoint: "https://example.test/openai/v1/chat/completions".into(),
            model: "m".into(),
            key: "sk-secret".into(),
            timeout: Duration::from_secs(60),
        };
        let command = sandbox(&settings, "ses_test");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().all(|arg| !arg.contains("sk-secret")),
            "{args:?}"
        );
        assert!(args.iter().all(|arg| !arg.contains("example.test")));
        assert!(args.iter().any(|arg| arg == CURL_SCRIPT));
        let envs: Vec<(String, String)> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.unwrap_or_default().to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert!(envs.contains(&("SGRAFFITO_AI_KEY".into(), "sk-secret".into())));
        assert!(envs.contains(&(
            "SGRAFFITO_AI_ENDPOINT".into(),
            "https://example.test/openai/v1/chat/completions".into()
        )));
        assert!(envs.contains(&("SGRAFFITO_AI_SESSION".into(), "ses_test".into())));
        assert!(envs.contains(&("SGRAFFITO_AI_TIMEOUT".into(), "60".into())));
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
