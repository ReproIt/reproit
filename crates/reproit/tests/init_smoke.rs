//! Bare `reproit init` smoke tests over the three field ground-truth
//! fixtures (validation/field/init-ground-truth.md): a stock Express-style
//! app with a query-param route, a raw node server with and without a
//! package.json, and an empty repo. The rule under test: zero flags, any
//! repo, exit 0 and a usable scaffold that `reproit list` can read back.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn reproit_bin() -> PathBuf {
    option_env!("CARGO_BIN_EXE_reproit")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_reproit")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("reproit-init-smoke-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_init(dir: &std::path::Path, timeout: Duration) -> (String, String, Option<i32>) {
    let mut cmd = Command::new(reproit_bin());
    cmd.arg("init")
        .current_dir(dir)
        // The tests assert the zero-flag path; a developer shell exporting a
        // target must not silently change what is being tested.
        .env_remove("REPROIT_BACKEND_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn reproit init");
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
                "init timed out\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_list(dir: &std::path::Path) -> (String, String, Option<i32>) {
    let out = Command::new(reproit_bin())
        .arg("list")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("run reproit list");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn assert_backend_scaffold(dir: &std::path::Path, stdout: &str, stderr: &str) {
    let config_path = dir.join("reproit.yaml");
    assert!(
        config_path.is_file(),
        "no reproit.yaml written\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let config = fs::read_to_string(&config_path).unwrap();
    assert!(
        config.contains("backend:\n  enabled: true"),
        "not a backend scaffold:\n{config}"
    );
    assert!(dir.join("openapi.yaml").is_file(), "no schema draft written");
    assert!(dir.join(".reproit/.gitignore").is_file());
    // The scaffold init writes must be readable by init's own sibling
    // commands: `reproit list` failed on it with "missing field 'app'".
    let (list_out, list_err, list_code) = run_list(dir);
    assert_eq!(
        list_code,
        Some(0),
        "reproit list rejected init's own config\nstdout:\n{list_out}\nstderr:\n{list_err}"
    );
}

/// A node fixture that node_ast derives express-style routes from AND that
/// plain `node server.js` can run with zero installed dependencies, so the
/// auto-boot enrichment is exercised wherever node exists.
const SERVER_JS: &str = r#"const http = require('http');
const handlers = [];
const app = {
  register(method, path, handler) { handlers.push({ method, path, handler }); },
  get(path, handler) { this.register('GET', path, handler); },
  post(path, handler) { this.register('POST', path, handler); },
};
app.get('/items', (req, res) => { res.end(JSON.stringify([{ id: 1, name: 'one' }])); });
app.get('/items/:id', (req, res) => { res.end(JSON.stringify({ id: 1, name: 'one' })); });
app.post('/items', (req, res) => { res.end(JSON.stringify({ ok: true })); });
app.get('/search', (req, res) => { res.end(JSON.stringify({ q: req.url, results: [] })); });
const server = http.createServer((req, res) => {
  const url = req.url.split('?')[0];
  res.setHeader('content-type', 'application/json');
  for (const entry of handlers) {
    const pattern = new RegExp('^' + entry.path.replace(/:[^/]+/g, '[^/]+') + '$');
    if (entry.method === req.method && pattern.test(url)) return entry.handler(req, res);
  }
  res.statusCode = 404;
  res.end('{}');
});
server.listen(process.env.PORT || 3000);
"#;

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[test]
fn express_style_fixture_scaffolds_derives_routes_and_auto_enriches() {
    let dir = temp_dir("express");
    fs::write(
        dir.join("package.json"),
        "{\n  \"name\": \"smoke-express\",\n  \"dependencies\": { \"express\": \"^4.19.2\" \
         },\n  \"scripts\": { \"start\": \"node server.js\" }\n}\n",
    )
    .unwrap();
    fs::write(dir.join("server.js"), SERVER_JS).unwrap();

    let (stdout, stderr, code) = run_init(&dir, Duration::from_secs(120));
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_backend_scaffold(&dir, &stdout, &stderr);
    let combined = format!("{stdout}\n{stderr}");
    // The summary counts operations on paths, matching the derivation line's
    // own counting scheme (the old "3 routes" wording contradicted it).
    assert!(
        combined.contains("4 operations on 3 paths"),
        "route derivation miscounted:\n{combined}"
    );
    let schema = fs::read_to_string(dir.join("openapi.yaml")).unwrap();
    assert!(schema.contains("/items/{id}"), "{schema}");
    assert!(schema.contains("/search"), "{schema}");
    if node_available() {
        // No target flag, no env var, no server started by the test: init
        // itself must boot the start script, enrich, and tear it down.
        assert!(
            combined.contains("enriched live") && !combined.contains("0 enriched live"),
            "auto-boot enrichment did not happen:\n{combined}"
        );
        if combined.contains("booting the package.json `start` script") {
            assert!(
                schema.contains("observed live during init"),
                "no observed response recorded:\n{schema}"
            );
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn raw_node_server_with_bare_package_json_degrades_to_backend_not_web() {
    let dir = temp_dir("raw-node-pkg");
    fs::write(
        dir.join("package.json"),
        "{ \"name\": \"raw\", \"scripts\": { \"start\": \"node server.js\" } }\n",
    )
    .unwrap();
    fs::write(
        dir.join("server.js"),
        "const http = require('http');\nhttp.createServer((req, res) => \
         res.end('ok')).listen(3000);\n",
    )
    .unwrap();

    let (stdout, stderr, code) = run_init(&dir, Duration::from_secs(30));
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_backend_scaffold(&dir, &stdout, &stderr);
    let config = fs::read_to_string(dir.join("reproit.yaml")).unwrap();
    // The old behavior: a silent web misclassification with a guessed URL and
    // a webRunnerDir that only exists in the reproit monorepo.
    assert!(!config.contains("platform: web"), "{config}");
    assert!(!config.contains("../reproit/runners/web"), "{config}");
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("assuming a backend service"),
        "the degrade path must state its assumption:\n{combined}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn raw_node_server_without_package_json_still_scaffolds() {
    let dir = temp_dir("raw-node-bare");
    fs::write(
        dir.join("server.js"),
        "const http = require('http');\nhttp.createServer((req, res) => \
         res.end('ok')).listen(3000);\n",
    )
    .unwrap();

    let (stdout, stderr, code) = run_init(&dir, Duration::from_secs(30));
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_backend_scaffold(&dir, &stdout, &stderr);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("add the routes"),
        "the degrade path must name the next input:\n{combined}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn empty_repo_still_scaffolds_and_rerun_stays_clean() {
    let dir = temp_dir("empty");

    let (stdout, stderr, code) = run_init(&dir, Duration::from_secs(30));
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_backend_scaffold(&dir, &stdout, &stderr);

    // Re-running bare init on an initialized project is a statement, not an
    // error: the scaffold exists, exit 0, --force stays the override.
    let (stdout, stderr, code) = run_init(&dir, Duration::from_secs(30));
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        format!("{stdout}\n{stderr}").contains("already exists"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}
