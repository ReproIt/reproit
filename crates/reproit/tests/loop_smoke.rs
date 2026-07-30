//! The turnkey loop over the ground-truth Express fixture
//! (validation/field/init-ground-truth.md): exactly three user commands,
//! `reproit init` -> `reproit find` -> `reproit keep`, must surface the
//! planted 500 as a CONFIRMED finding and end in a committed-ready guard plus
//! CI wiring, with `reproit check` then failing while the bug exists and
//! passing once it is fixed. Self-gated on node availability, following
//! init_smoke.rs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn reproit_bin() -> PathBuf {
    option_env!("CARGO_BIN_EXE_reproit")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_reproit")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("reproit-loop-smoke-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Run one reproit command in `dir` with a hard timeout, returning
/// (stdout, stderr, exit code). The zero-flag loop is the subject, so an
/// inherited target env var must not silently change what is tested.
fn run_reproit(dir: &Path, args: &[&str], timeout: Duration) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(reproit_bin());
    cmd.args(args)
        .current_dir(dir)
        .env_remove("REPROIT_BACKEND_URL")
        .env_remove("REPROIT_BACKEND_RESET_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn reproit");
    let start = Instant::now();
    loop {
        if child.try_wait().expect("poll child").is_some() {
            let out = child.wait_with_output().expect("collect output");
            return (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                out.status.code(),
            );
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let out = child.wait_with_output().expect("collect killed output");
            panic!(
                "reproit {args:?} timed out\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The ground-truth fixture A shape: express-style routes node_ast derives,
/// runnable by plain `node server.js` with zero installed dependencies.
/// The planted bug: POST /items without `name` throws (`name.trim()` on
/// undefined) and answers HTTP 500 with a stack trace, express-style.
fn server_js(fixed: bool) -> String {
    let post_body = if fixed {
        // The fix: a missing name is rejected with a 400, never a 500.
        "    if (typeof body.name !== 'string') {\n      res.statusCode = 400;\n      \
         return res.end(JSON.stringify({ error: 'name is required' }));\n    }\n    \
         res.end(JSON.stringify({ id: 2, name: body.name.trim() }));"
    } else {
        "    res.end(JSON.stringify({ id: 2, name: body.name.trim() }));"
    };
    format!(
        r#"const http = require('http');
const handlers = [];
const app = {{
  register(method, path, handler) {{ handlers.push({{ method, path, handler }}); }},
  get(path, handler) {{ this.register('GET', path, handler); }},
  post(path, handler) {{ this.register('POST', path, handler); }},
}};
app.get('/items', (req, res) => {{ res.end(JSON.stringify([{{ id: 1, name: 'one' }}])); }});
app.get('/items/:id', (req, res) => {{ res.end(JSON.stringify({{ id: 1, name: 'one' }})); }});
app.post('/items', (req, res) => {{
  let raw = '';
  req.on('data', (chunk) => {{ raw += chunk; }});
  req.on('end', () => {{
    let body = {{}};
    try {{ body = JSON.parse(raw || '{{}}'); }} catch (error) {{ body = {{}}; }}
    try {{
{post_body}
    }} catch (error) {{
      res.statusCode = 500;
      res.setHeader('content-type', 'text/html; charset=utf-8');
      res.end('<!DOCTYPE html><pre>' + error.stack + '</pre>');
    }}
  }});
}});
const server = http.createServer((req, res) => {{
  const url = req.url.split('?')[0];
  res.setHeader('content-type', 'application/json');
  for (const entry of handlers) {{
    const pattern = new RegExp('^' + entry.path.replace(/:[^/]+/g, '[^/]+') + '$');
    if (entry.method === req.method && pattern.test(url)) return entry.handler(req, res);
  }}
  res.statusCode = 404;
  res.end('{{}}');
}});
server.listen(process.env.PORT || 3000);
"#
    )
}

fn write_fixture(dir: &Path, fixed: bool) {
    fs::write(
        dir.join("package.json"),
        "{\n  \"name\": \"loop-express\",\n  \"dependencies\": { \"express\": \"^4.19.2\" \
         },\n  \"scripts\": { \"start\": \"node server.js\" }\n}\n",
    )
    .unwrap();
    fs::write(dir.join("server.js"), server_js(fixed)).unwrap();
}

/// The confirmed-finding count a backend engine line reports, e.g.
/// `backend fuzz: 12 operation(s) exercised, 1 confirmed finding(s), ...`.
fn confirmed_findings(output: &str, engine: &str) -> Option<u64> {
    let prefix = format!("backend {engine}:");
    output.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with(&prefix) {
            return None;
        }
        let (before, _) = line.split_once(" confirmed finding(s)")?;
        before.rsplit(' ').next()?.parse().ok()
    })
}

#[test]
fn three_command_loop_confirms_keeps_and_checks_the_planted_500() {
    if !node_available() {
        eprintln!("skipping: node is not available");
        return;
    }
    let dir = temp_dir("express");
    write_fixture(&dir, false);

    // Command 1: bare init scaffolds and enriches with zero flags.
    let (stdout, stderr, code) = run_reproit(&dir, &["init"], Duration::from_secs(120));
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(dir.join("reproit.yaml").is_file());

    // Command 2: bare find boots the service itself, routes the mutation
    // routes through fuzz, and CONFIRMS the planted 500 (no reset URL, no
    // target flag). Exit 1 is the regression signal: a bug was found.
    let (stdout, stderr, code) = run_reproit(&dir, &["find"], Duration::from_secs(300));
    let combined = format!("{stdout}\n{stderr}");
    assert_eq!(code, Some(1), "find should report the bug:\n{combined}");
    let confirmed = confirmed_findings(&combined, "fuzz")
        .unwrap_or_else(|| panic!("no backend fuzz summary line:\n{combined}"));
    assert!(
        confirmed >= 1,
        "the planted 500 must be a CONFIRMED finding, not a candidate:\n{combined}"
    );

    // Command 3: bare keep writes the committed guard AND the CI wiring.
    let (stdout, stderr, code) = run_reproit(&dir, &["keep"], Duration::from_secs(60));
    let combined = format!("{stdout}\n{stderr}");
    assert_eq!(code, Some(0), "keep failed:\n{combined}");
    let repros = dir.join(".reproit/repros");
    let guard = fs::read_dir(&repros)
        .expect("guard store exists")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.join("backend.json").is_file())
        .unwrap_or_else(|| {
            panic!(
                "no kept backend guard under {}:\n{combined}",
                repros.display()
            )
        });
    assert!(guard.join("meta.json").is_file());
    let workflow = dir.join(".github/workflows/reproit.yml");
    let workflow_body = fs::read_to_string(&workflow).expect("CI workflow written");
    assert!(
        workflow_body.contains("run: reproit check"),
        "{workflow_body}"
    );
    assert!(
        workflow_body.contains("0 pass, 1 regression, 2 flaky, 3 stale"),
        "{workflow_body}"
    );

    // `reproit check` against the BROKEN app: the guard reproduces, exit 1.
    let (stdout, stderr, code) = run_reproit(&dir, &["check"], Duration::from_secs(180));
    let combined = format!("{stdout}\n{stderr}");
    assert_eq!(
        code,
        Some(1),
        "check must fail while the bug exists:\n{combined}"
    );
    assert!(
        combined.contains("REPRODUCED"),
        "the guard must name the returning bug:\n{combined}"
    );

    // Fix the app: `reproit check` passes and the guard is proven held.
    write_fixture(&dir, true);
    let (stdout, stderr, code) = run_reproit(&dir, &["check"], Duration::from_secs(180));
    let combined = format!("{stdout}\n{stderr}");
    assert_eq!(code, Some(0), "check must pass once fixed:\n{combined}");
    assert!(
        combined.contains("held (does not reproduce)"),
        "the guard must be proven held:\n{combined}"
    );
    let _ = fs::remove_dir_all(&dir);
}
