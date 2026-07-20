//! PTY lifecycle and terminal query handling for one target session.

use super::interaction::{observe_mouse_protocol_stream, MouseProtocol};
use super::*;

pub(super) fn looks_crashed(parser: &Arc<Mutex<vt100::Parser>>) -> bool {
    let contents = parser.lock().unwrap().screen().contents();
    contents.contains("panicked at")
        || contents.contains("Traceback (most recent call last)")
        || contents.contains("thread 'main' panicked")
}

/// Full-screen TUIs (helix, lazygit, k9s, Claude Code) probe the terminal at
/// startup and BLOCK rendering until they get answers. A dumb PTY never
/// replies, so they stall at a blank screen. We scan the app's output for the
/// common queries and write canned responses back, so the app proceeds and
/// renders.
fn answer_queries(
    chunk: &[u8],
    parser: &Arc<Mutex<vt100::Parser>>,
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
) {
    let mut resp: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i + 2 < chunk.len() {
        if chunk[i] == 0x1b && chunk[i + 1] == b'[' {
            let rest = &chunk[i..];
            if rest.starts_with(b"\x1b[c") || rest.starts_with(b"\x1b[0c") {
                // Primary Device Attributes -> claim a VT220-class terminal.
                resp.extend_from_slice(b"\x1b[?62;22c");
            } else if rest.starts_with(b"\x1b[>c") || rest.starts_with(b"\x1b[>0c") {
                // Secondary Device Attributes -> a plausible xterm identity.
                resp.extend_from_slice(b"\x1b[>0;276;0c");
            } else if rest.starts_with(b"\x1b[5n") {
                // Device status report -> OK.
                resp.extend_from_slice(b"\x1b[0n");
            } else if rest.starts_with(b"\x1b[6n") {
                // Cursor position report -> the parser's current cursor (1-based).
                let (row, col) = parser.lock().unwrap().screen().cursor_position();
                resp.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            } else if rest.starts_with(b"\x1b[?u") {
                // Kitty keyboard protocol query -> report "supported, 0 flags".
                resp.extend_from_slice(b"\x1b[?0u");
            } else if rest.starts_with(b"\x1b[?2026$p") {
                // DECRQM for synchronized output -> reset/not active.
                resp.extend_from_slice(b"\x1b[?2026;2$y");
            } else if rest.starts_with(b"\x1b[>q") {
                // XTVERSION -> a terminal name/version string.
                resp.extend_from_slice(b"\x1bP>|reproit(0.1)\x1b\\");
            }
        }
        i += 1;
    }
    if !resp.is_empty() {
        if let Ok(mut w) = writer.lock() {
            let _ = w.write_all(&resp);
            let _ = w.flush();
        }
    }
}

/// Count the full-screen ERASE-DISPLAY sequences (`CSI 2 J` / `CSI 3 J`) in a
/// raw output chunk. An app that clears the WHOLE screen and redraws it on a
/// keystroke is doing a full re-render; a well-behaved TUI (ncurses optimized
/// output, ratatui's diffing renderer) emits targeted cell updates and almost
/// never a full ED. So a full ED in response to an action is the deterministic
/// byte-stream signature of a wasteful full repaint, the VT analogue of the
/// web runner's node-identity churn. We count both `2J` (erase all) and `3J`
/// (erase all + scrollback); `0J`/`1J` (erase to end/start) are partial and not
/// counted. Pure scan, so the same app output yields the same count on replay.
pub(super) fn count_full_erases(chunk: &[u8]) -> u64 {
    let mut n = 0u64;
    let mut i = 0usize;
    while i + 3 < chunk.len() {
        if chunk[i] == 0x1b
            && chunk[i + 1] == b'['
            && (chunk[i + 2] == b'2' || chunk[i + 2] == b'3')
            && chunk[i + 3] == b'J'
        {
            n += 1;
            i += 4;
        } else {
            i += 1;
        }
    }
    n
}

type Session = (
    Box<dyn portable_pty::MasterPty + Send>,
    Box<dyn portable_pty::Child + Send + Sync>,
    Arc<Mutex<vt100::Parser>>,
    Arc<Mutex<Box<dyn Write + Send>>>,
    // Running count of full-screen ERASE-DISPLAY sequences the app has emitted.
    // Sampled before/after each keystroke so the re-render oracle can tell when
    // an action triggered a full clear+redraw.
    Arc<AtomicU64>,
    // Mouse encoding requested by the app. A terminal consumes DECSET mode
    // changes; they must never be echoed back as app input.
    Arc<AtomicU8>,
);

/// Open a PTY, launch the target via `sh -c`, start a reader thread feeding a
/// fresh VT parser, and return the handles. Called once per session: we
/// relaunch on a clean app exit so a quit key doesn't end fuzzing early.
pub(super) fn spawn_session(cmdline: &str) -> Result<Session> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows: ROWS,
        cols: COLS,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    // The OS shell to interpret the command line (args + PATH resolution). On unix,
    // `sh -c "exec <cmd>"`: `exec` REPLACES the shell with the app at the same pid,
    // so child.process_id() is reliably the app's, not the wrapping `sh`'s -- the
    // --soak RSS sampler keys on that pid (most shells auto-exec a single simple
    // command, but that's an optimization, not guaranteed). On Windows there is no
    // `sh`/`exec`, so `cmd /c <cmd>` (the app runs as cmd's child; the /proc-based
    // RSS sampler is unix-only regardless, so this only affects --soak there).
    #[cfg(windows)]
    let (shell, flag, line) = ("cmd", "/c", cmdline.to_string());
    #[cfg(not(windows))]
    let (shell, flag, line) = ("sh", "-c", format!("exec {cmdline}"));
    let mut cmd = CommandBuilder::new(shell);
    cmd.arg(flag);
    cmd.arg(line);
    if let Some(cwd) = std::env::var_os("REPROIT_TUI_CWD").filter(|p| !p.is_empty()) {
        cmd.cwd(cwd);
    }
    cmd.env("TERM", "xterm-256color");
    // App-invariant channel + fuzzer-detection gate: the SDK writes its
    // REPROIT_INVARIANT markers to this file (see InvariantScrape) and, seeing
    // the var, evaluates its registry; absent (production) the registry is inert.
    cmd.env("REPROIT_INVARIANT_FILE", marker_file_path());
    cmd.env("REPROIT_INPUTS_FILE", input_file_path());
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(pair.master.take_writer()?));
    let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
    let erases = Arc::new(AtomicU64::new(0));
    let mouse = Arc::new(AtomicU8::new(MouseProtocol::None as u8));
    {
        let parser = parser.clone();
        let writer = writer.clone();
        let erases = erases.clone();
        let mouse = mouse.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut terminal_mode_tail = Vec::with_capacity(15);
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let e = count_full_erases(&buf[..n]);
                if e > 0 {
                    erases.fetch_add(e, Ordering::Relaxed);
                }
                observe_mouse_protocol_stream(&buf[..n], &mut terminal_mode_tail, &mouse);
                parser.lock().unwrap().process(&buf[..n]);
                answer_queries(&buf[..n], &parser, &writer);
            }
        });
    }
    Ok((pair.master, child, parser, writer, erases, mouse))
}
