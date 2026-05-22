//! Embedded browser pane: the CDP screencast + input session (oh-my-warp).
//!
//! Strategy 1 from `BROWSER_PANE_SPEC.md`: we do *not* embed a browser engine.
//! We spawn a real Chrome with the DevTools remote-debugging endpoint, drive the
//! Chrome DevTools Protocol's `Page.startScreencast`, stream the resulting JPEG
//! frames back to a GPUI pane, and forward the user's mouse/scroll/navigation
//! back over CDP `Input.*` / `Page.*`.
//!
//! Everything runs on a dedicated OS thread (blocking `tungstenite`) decoupled
//! from the UI thread:
//! - **frames + URL changes** flow out via the `update_tx` channel (drained by
//!   the view on the foreground executor),
//! - **commands** (navigate / click / scroll / back / forward / reload) flow in
//!   via the `cmd` channel, drained each loop iteration.
//!
//! A short socket read timeout lets the single thread interleave reading frames
//! with sending queued commands without a second socket.
//!
//! ## Screencast protocol notes (verified against Chrome)
//! - Frames arrive as `Page.screencastFrame` (base64 JPEG + a `sessionId`) and
//!   **each must be acked** (`Page.screencastFrameAck`) or the stream stalls.
//! - Screencast is **change-driven**; an idle page costs nothing.
//! - Input coordinates are CSS pixels relative to the viewport, so the frame
//!   carries `css_width`/`css_height` for the view to map clicks into.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use tungstenite::{Message, WebSocket};
use warpui::image_cache::StaticImage;

/// The fallback page a freshly-opened browser pane navigates to when no start
/// page is configured (see [`BrowserConfig`]).
pub const DEFAULT_URL: &str = "https://example.com";

/// User-configurable browser-pane settings, from `~/.warp/oh-my-warp.toml`'s
/// `[browser]` table (with `OMW_BROWSER_*` env overrides).
pub struct BrowserConfig {
    /// The start page (`[browser] home`, `OMW_BROWSER_HOME`).
    pub home: String,
    /// Invert scroll-wheel direction (`[browser] reverse_scroll`,
    /// `OMW_BROWSER_REVERSE_SCROLL`).
    pub reverse_scroll: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            home: DEFAULT_URL.to_owned(),
            reverse_scroll: false,
        }
    }
}

/// Loads [`BrowserConfig`] from `~/.warp/oh-my-warp.toml` (`[browser]` table),
/// then applies `OMW_BROWSER_*` environment overrides.
pub fn load_config() -> BrowserConfig {
    let mut config = BrowserConfig::default();

    if let Some(home_dir) = std::env::var_os("HOME") {
        let path = std::path::Path::new(&home_dir).join(".warp/oh-my-warp.toml");
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(value) = contents.parse::<toml::Value>() {
                if let Some(browser) = value.get("browser") {
                    if let Some(home) = browser.get("home").and_then(|v| v.as_str()) {
                        let home = home.trim();
                        if !home.is_empty() {
                            config.home = home.to_owned();
                        }
                    }
                    if let Some(reverse) = browser.get("reverse_scroll").and_then(|v| v.as_bool()) {
                        config.reverse_scroll = reverse;
                    }
                }
            }
        }
    }

    if let Ok(home) = std::env::var("OMW_BROWSER_HOME") {
        let home = home.trim();
        if !home.is_empty() {
            config.home = home.to_owned();
        }
    }
    if let Ok(reverse) = std::env::var("OMW_BROWSER_REVERSE_SCROLL") {
        config.reverse_scroll = matches!(reverse.trim(), "1" | "true" | "yes");
    }

    config
}

/// macOS path to the system Chrome binary that hosts the CDP endpoint.
const CHROME_PATH: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

/// How long the read blocks before we wake to drain queued input commands.
const READ_TIMEOUT: Duration = Duration::from_millis(16);

/// A decoded viewport frame plus the CSS viewport size it represents (used by
/// the view to map pane-local clicks into CDP input coordinates).
pub struct ViewportFrame {
    pub image: Arc<StaticImage>,
    pub css_width: f32,
    pub css_height: f32,
}

/// An update produced by the session thread for the view to apply.
pub enum SessionUpdate {
    /// A freshly decoded viewport frame.
    Frame(ViewportFrame),
    /// The main frame navigated; carries the new URL (updates the address bar).
    Url(String),
}

/// A command from the view to the session thread, translated to CDP.
/// Mouse coordinates are already in CSS viewport pixels.
#[derive(Clone)]
pub enum BrowserCommand {
    Navigate(String),
    Reload,
    Back,
    Forward,
    MouseMove {
        x: f64,
        y: f64,
    },
    MouseDown {
        x: f64,
        y: f64,
        click_count: i64,
    },
    MouseUp {
        x: f64,
        y: f64,
        click_count: i64,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    /// Resize the browser viewport (CSS px) to match the pane, so the page fills
    /// it instead of letterboxing.
    Resize {
        width: u32,
        height: u32,
    },
    /// Insert literal text into the focused page element (typing).
    InsertText(String),
    /// Press + release a named key in the page (Enter, Backspace, arrows, …).
    KeyPress {
        key: String,
        code: String,
        vk: i64,
    },
}

/// A single CDP DevTools target as reported by `GET /json`.
#[derive(Deserialize)]
struct CdpTarget {
    #[serde(rename = "type")]
    target_type: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    ws_url: Option<String>,
}

/// Owns a spawned Chrome process and the background thread streaming its
/// viewport / accepting input. Dropping it stops the thread and kills Chrome.
pub struct BrowserSession {
    child: Option<Child>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl BrowserSession {
    /// Spawns Chrome pointed at `url`, then starts a background thread that
    /// connects to its CDP endpoint, pushes updates into `update_tx`, and drains
    /// commands from `cmd_rx`. Returns immediately. The caller owns the command
    /// `Sender` (so input/UI closures can send without holding the session).
    pub fn spawn(
        url: &str,
        update_tx: async_channel::Sender<SessionUpdate>,
        cmd_rx: async_channel::Receiver<BrowserCommand>,
    ) -> Result<Self> {
        let port = free_port()?;
        let profile = std::env::temp_dir().join(format!("omw-browser-{port}"));

        let child = Command::new(CHROME_PATH)
            .arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile.display()))
            // Accept the CDP WebSocket upgrade regardless of Origin.
            .arg("--remote-allow-origins=*")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            // Keep the tab live so the compositor keeps emitting frames.
            .arg("--disable-background-timer-throttling")
            .arg("--disable-backgrounding-occluded-windows")
            .arg("--disable-renderer-backgrounding")
            // Headless so no separate Chrome window appears; the anti-throttle
            // flags above keep the (windowless) tab compositing so screencast
            // frames keep flowing. --window-size sets the viewport dimensions.
            .arg("--headless=new")
            .arg("--window-size=1100,800")
            .arg(url)
            .spawn()
            .with_context(|| format!("failed to spawn Chrome at {CHROME_PATH}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let thread = std::thread::Builder::new()
            .name("omw-browser-cdp".to_owned())
            .spawn(move || {
                if let Err(e) = run_session(port, &update_tx, &cmd_rx, &stop_thread) {
                    log::warn!("[omw-browser] CDP session ended: {e:#}");
                }
            })
            .context("failed to spawn browser CDP thread")?;

        log::info!("[omw-browser] spawned Chrome on CDP port {port} → {url}");
        Ok(Self {
            child: Some(child),
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Killing Chrome closes the CDP socket, which unblocks the thread's read
        // so it exits on its own; we don't join (avoid stalling the UI thread).
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        drop(self.thread.take());
    }
}

/// Asks the OS for an unused TCP port (bind to :0, read it back, drop).
fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").context("bind ephemeral port")?;
    Ok(listener.local_addr()?.port())
}

/// Polls `GET /json` until a `page` target with a WebSocket URL is available.
fn discover_target(port: u16, stop: &AtomicBool) -> Result<String> {
    let url = format!("http://127.0.0.1:{port}/json");
    for _ in 0..150 {
        if stop.load(Ordering::SeqCst) {
            return Err(anyhow!("stopped before CDP target was ready"));
        }
        if let Ok(resp) = reqwest::blocking::get(&url) {
            if let Ok(targets) = resp.json::<Vec<CdpTarget>>() {
                if let Some(ws) = targets
                    .into_iter()
                    .find(|t| t.target_type == "page" && t.ws_url.is_some())
                    .and_then(|t| t.ws_url)
                {
                    return Ok(ws);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(anyhow!("no CDP page target appeared on port {port}"))
}

/// Sends a CDP command, auto-incrementing the request id.
fn cdp_send(
    socket: &mut WebSocket<TcpStream>,
    next_id: &mut i64,
    method: &str,
    params: Value,
) -> Result<()> {
    *next_id += 1;
    let msg = json!({ "id": *next_id, "method": method, "params": params });
    socket
        .send(Message::Text(msg.to_string()))
        .map_err(|e| anyhow!("CDP send {method}: {e}"))
}

/// Starts (or restarts) the JPEG screencast.
fn start_screencast(socket: &mut WebSocket<TcpStream>, next_id: &mut i64) -> Result<()> {
    cdp_send(
        socket,
        next_id,
        "Page.startScreencast",
        json!({ "format": "jpeg", "quality": 70, "maxWidth": 1600, "maxHeight": 1000, "everyNthFrame": 1 }),
    )
}

/// Translates one [`BrowserCommand`] into the corresponding CDP message(s).
fn apply_command(
    socket: &mut WebSocket<TcpStream>,
    next_id: &mut i64,
    cmd: BrowserCommand,
) -> Result<()> {
    match cmd {
        BrowserCommand::Navigate(url) => {
            cdp_send(socket, next_id, "Page.navigate", json!({ "url": url }))
        }
        BrowserCommand::Reload => cdp_send(socket, next_id, "Page.reload", json!({})),
        BrowserCommand::Back => cdp_send(
            socket,
            next_id,
            "Runtime.evaluate",
            json!({ "expression": "history.back()" }),
        ),
        BrowserCommand::Forward => cdp_send(
            socket,
            next_id,
            "Runtime.evaluate",
            json!({ "expression": "history.forward()" }),
        ),
        BrowserCommand::MouseMove { x, y } => cdp_send(
            socket,
            next_id,
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0 }),
        ),
        BrowserCommand::MouseDown { x, y, click_count } => {
            // Move to the point first so hover/target state is set, then press.
            cdp_send(
                socket,
                next_id,
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0 }),
            )?;
            cdp_send(
                socket,
                next_id,
                "Input.dispatchMouseEvent",
                json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "buttons": 1, "clickCount": click_count }),
            )
        }
        BrowserCommand::MouseUp { x, y, click_count } => cdp_send(
            socket,
            next_id,
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "buttons": 0, "clickCount": click_count }),
        ),
        BrowserCommand::Wheel {
            x,
            y,
            delta_x,
            delta_y,
        } => cdp_send(
            socket,
            next_id,
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": delta_x, "deltaY": delta_y }),
        ),
        BrowserCommand::Resize { width, height } => {
            log::info!("[omw-browser] resize viewport to {width}x{height}");
            cdp_send(
                socket,
                next_id,
                "Emulation.setDeviceMetricsOverride",
                json!({
                    "width": width,
                    "height": height,
                    "deviceScaleFactor": 2,
                    "mobile": false,
                }),
            )
        }
        BrowserCommand::InsertText(text) => {
            cdp_send(socket, next_id, "Input.insertText", json!({ "text": text }))
        }
        BrowserCommand::KeyPress { key, code, vk } => {
            cdp_send(
                socket,
                next_id,
                "Input.dispatchKeyEvent",
                json!({ "type": "keyDown", "key": key, "code": code, "windowsVirtualKeyCode": vk }),
            )?;
            cdp_send(
                socket,
                next_id,
                "Input.dispatchKeyEvent",
                json!({ "type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk }),
            )
        }
    }
}

/// Connects to the CDP endpoint, starts the screencast, and pumps frames out /
/// commands in until the socket closes or `stop` is set.
fn run_session(
    port: u16,
    update_tx: &async_channel::Sender<SessionUpdate>,
    cmd_rx: &async_channel::Receiver<BrowserCommand>,
    stop: &AtomicBool,
) -> Result<()> {
    let ws_url = discover_target(port, stop)?;
    let stream = TcpStream::connect(("127.0.0.1", port)).context("connect CDP TCP socket")?;
    stream.set_nodelay(true).ok();
    let (mut socket, _resp) = tungstenite::client::client(ws_url.as_str(), stream)
        .map_err(|e| anyhow!("CDP WebSocket handshake failed: {e}"))?;
    // After the (blocking) handshake, switch to a short read timeout so the loop
    // can interleave reads with draining the command channel.
    socket
        .get_mut()
        .set_read_timeout(Some(READ_TIMEOUT))
        .context("set CDP read timeout")?;

    let mut next_id = 0i64;
    cdp_send(&mut socket, &mut next_id, "Page.enable", json!({}))?;
    start_screencast(&mut socket, &mut next_id)?;
    log::info!("[omw-browser] screencast started on port {port}");

    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }

        // Drain any queued input/navigation commands first (low latency),
        // coalescing consecutive mouse-moves to the latest so rapid hover
        // movement doesn't flood CDP (clicks/scrolls flush any pending move).
        let mut pending_move: Option<BrowserCommand> = None;
        let mut pending_resize: Option<BrowserCommand> = None;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                BrowserCommand::MouseMove { .. } => pending_move = Some(cmd),
                BrowserCommand::Resize { .. } => pending_resize = Some(cmd),
                other => {
                    // Resize first (changes the coordinate space), then move.
                    if let Some(rs) = pending_resize.take() {
                        apply_command(&mut socket, &mut next_id, rs)?;
                    }
                    if let Some(mv) = pending_move.take() {
                        apply_command(&mut socket, &mut next_id, mv)?;
                    }
                    apply_command(&mut socket, &mut next_id, other)?;
                }
            }
        }
        if let Some(rs) = pending_resize.take() {
            apply_command(&mut socket, &mut next_id, rs)?;
        }
        if let Some(mv) = pending_move.take() {
            apply_command(&mut socket, &mut next_id, mv)?;
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                if update_tx.is_closed() {
                    return Ok(()); // view dropped
                }
                handle_cdp_message(&mut socket, &mut next_id, &text, update_tx)?;
            }
            Ok(Message::Ping(payload)) => {
                socket
                    .send(Message::Pong(payload))
                    .map_err(|e| anyhow!("CDP pong: {e}"))?;
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            // Read timeout: no frame waiting — loop back to drain commands.
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(anyhow!("CDP read: {e}")),
        }
    }
}

/// Handles one CDP event message: screencast frames (decoded + acked) and main
/// frame navigations (URL updates).
fn handle_cdp_message(
    socket: &mut WebSocket<TcpStream>,
    next_id: &mut i64,
    text: &str,
    update_tx: &async_channel::Sender<SessionUpdate>,
) -> Result<()> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    match value.get("method").and_then(Value::as_str) {
        Some("Page.screencastFrame") => {
            let params = &value["params"];
            let Some(data_b64) = params["data"].as_str() else {
                return Ok(());
            };
            let session_id = params["sessionId"].clone();
            let css_width = params["metadata"]["deviceWidth"].as_f64().unwrap_or(0.0) as f32;
            let css_height = params["metadata"]["deviceHeight"].as_f64().unwrap_or(0.0) as f32;

            match decode_frame(data_b64) {
                Ok(image) => {
                    let _ = update_tx.try_send(SessionUpdate::Frame(ViewportFrame {
                        image,
                        css_width,
                        css_height,
                    }));
                }
                Err(e) => log::warn!("[omw-browser] frame decode failed: {e:#}"),
            }

            // Ack so Chrome sends the next frame (backpressure).
            cdp_send(
                socket,
                next_id,
                "Page.screencastFrameAck",
                json!({ "sessionId": session_id }),
            )
        }
        Some("Page.frameNavigated") => {
            let frame = &value["params"]["frame"];
            // Main frame only (no parentId).
            if frame.get("parentId").is_none() {
                if let Some(url) = frame["url"].as_str() {
                    let _ = update_tx.try_send(SessionUpdate::Url(url.to_owned()));
                }
                // Screencast can stop across a navigation; restart it so the new
                // page keeps streaming (e.g. after following a link).
                start_screencast(socket, next_id)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Decodes a base64 JPEG screencast frame into an `Arc<StaticImage>`.
fn decode_frame(data_b64: &str) -> Result<Arc<StaticImage>> {
    let jpeg = BASE64.decode(data_b64).context("base64 decode")?;
    let rgba = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg)
        .context("jpeg decode")?
        .into_rgba8();
    Ok(Arc::new(StaticImage::from_rgba(rgba)))
}
