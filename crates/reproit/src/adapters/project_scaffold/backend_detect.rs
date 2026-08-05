//! Backend framework detection from project manifests, plus the per-framework
//! schema guidance and adapter one-liner that `init` and `doctor` teach.
//! Detection is a hint for guidance only; it never creates configuration.

use std::path::{Path, PathBuf};

/// Manifests are small; anything larger is not a manifest we should parse.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_DOTNET_PROJECT_SCAN: usize = 128;
const MAX_DOTNET_PROJECT_DEPTH: usize = 4;
const DOTNET_WEB_SDKS: [&str; 3] = [
    "Microsoft.NET.Sdk.Web",
    "Microsoft.NET.Sdk.Razor",
    "Microsoft.NET.Sdk.BlazorWebAssembly",
];
const DOTNET_SKIP_DIRS: [&str; 6] = ["bin", "obj", "node_modules", "vendor", "build", ".git"];

/// One detected backend framework: where it was seen, how to get a schema out
/// of it, and the one-line ReproIt adapter mount for effect-level verdicts.
pub struct BackendFramework {
    pub name: &'static str,
    pub manifest: &'static str,
    pub schema_hint: &'static str,
    pub adapter_snippet: &'static str,
}

const fn framework(
    name: &'static str,
    manifest: &'static str,
    schema_hint: &'static str,
    adapter_snippet: &'static str,
) -> BackendFramework {
    BackendFramework {
        name,
        manifest,
        schema_hint,
        adapter_snippet,
    }
}

const NODE_EXPRESS_SNIPPET: &str = "app.use(require('reproit-backend-node/express')({ capture }))";
const PY_ASGI_SNIPPET: &str = "app.add_middleware(ReproitMiddleware, capture=capture)";
const RS_AXUM_SNIPPET: &str =
    ".layer(ReproitLayer::new(MiddlewareConfig { capture, ..MiddlewareConfig::default() }))";
const UTOIPA_HINT: &str = "expose an OpenAPI schema with utoipa (utoipa-swagger-ui serves \
                           /api-docs/openapi.json), then `reproit init <that url>`";
const SWAG_HINT: &str = "generate an OpenAPI schema with swaggo/swag (`swag init` writes \
                         docs/swagger.json), then `reproit init docs/swagger.json`... or point \
                         `reproit init` at a served /swagger/doc.json URL";

fn rust_framework(name: &'static str) -> BackendFramework {
    let snippet = match name {
        "actix-web" => {
            ".wrap(Reproit::new(MiddlewareConfig { capture, ..MiddlewareConfig::default() }))"
        }
        _ => RS_AXUM_SNIPPET,
    };
    let hint = match name {
        "rocket" => {
            "expose an OpenAPI schema with rocket_okapi (serves /openapi.json), then \
                     `reproit init http://localhost:8000/openapi.json`"
        }
        _ => UTOIPA_HINT,
    };
    framework(name, "Cargo.toml", hint, snippet)
}

fn node_framework(name: &'static str) -> BackendFramework {
    let snippet = match name {
        "fastify" => "fastify.register(require('reproit-backend-node/fastify'), { capture })",
        // There is no drop-in adapter for a raw node server: the express
        // middleware is `(req, res, next)`-shaped, so a createServer handler
        // calls it directly. Naming the real shape beats naming a mount that
        // does not exist.
        "node:http" => {
            "require('reproit-backend-node/express')({ capture }), called from your \
             createServer handler"
        }
        _ => NODE_EXPRESS_SNIPPET,
    };
    let hint = match name {
        "node:http" => {
            "hand-write openapi.yaml for the routes you serve, then `reproit init \
                      openapi.yaml`"
        }
        "fastify" => {
            "register @fastify/swagger (serves /documentation/json), then `reproit init \
                      http://localhost:3000/documentation/json`"
        }
        "nestjs" => {
            "add @nestjs/swagger and SwaggerModule (serves /api-json), then `reproit init \
                      http://localhost:3000/api-json`"
        }
        _ => {
            "generate an OpenAPI schema with swagger-jsdoc (serve it or write openapi.yaml), or \
              hand-write openapi.yaml, then `reproit init <schema url or file>`"
        }
    };
    framework(name, "package.json", hint, snippet)
}

fn python_framework(name: &'static str, manifest: &'static str) -> BackendFramework {
    let hint = match name {
        "fastapi" => {
            "FastAPI already serves its schema: `reproit init \
                      http://localhost:8000/openapi.json`"
        }
        "django" => {
            "django-ninja serves /api/openapi.json; Django REST needs drf-spectacular \
                     (serves /api/schema/). Then `reproit init <that url>`"
        }
        _ => {
            "add flask-smorest (serves /api/openapi.json) or hand-write openapi.yaml, then \
              `reproit init <schema url or file>`"
        }
    };
    framework(name, manifest, hint, PY_ASGI_SNIPPET)
}

fn go_framework(name: &'static str) -> BackendFramework {
    framework(
        name,
        "go.mod",
        SWAG_HINT,
        "handler := reproit.Middleware(reproit.MiddlewareOptions{Capture: capture})(mux)",
    )
}

/// Detect the backend framework of a project directory from its manifests.
/// Returns None for UI projects and anything unrecognized. package.json is
/// treated as a backend only when it has a server framework and no obvious
/// ASP.NET Core, detected from a `.csproj`.
///
/// A solution marker permits a bounded search for nested projects, but is not
/// itself evidence of a web service. The project SDK is still authoritative:
/// plain library, console, worker, test, and app-host projects remain excluded.
fn dotnet_backend(dir: &Path) -> Option<BackendFramework> {
    if direct_dotnet_web_project(dir) {
        return Some(dotnet_framework("csproj"));
    }
    let marker = dotnet_root_marker(dir)?;
    let mut remaining = MAX_DOTNET_PROJECT_SCAN;
    nested_dotnet_web_project(dir, 0, &mut remaining).then(|| dotnet_framework(marker))
}

fn dotnet_framework(manifest: &'static str) -> BackendFramework {
    framework(
        "aspnet",
        manifest,
        "expose an OpenAPI schema with Swashbuckle or the built-in \
         `AddOpenApi()` (serves /swagger/v1/swagger.json or /openapi/v1.json), \
         then `reproit init <that url>`",
        "app.UseMiddleware<ReproitMiddleware>(capture)",
    )
}

fn direct_dotnet_web_project(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .any(|path| is_dotnet_web_project(&path))
}

fn is_dotnet_web_project(path: &Path) -> bool {
    if path.extension().and_then(|extension| extension.to_str()) != Some("csproj") {
        return false;
    }
    let readable = std::fs::metadata(path)
        .ok()
        .is_some_and(|metadata| metadata.is_file() && metadata.len() <= MAX_MANIFEST_BYTES);
    if !readable {
        return false;
    }
    let Ok(project) = std::fs::read_to_string(path) else {
        return false;
    };
    DOTNET_WEB_SDKS.iter().any(|sdk| project.contains(sdk))
}

fn dotnet_root_marker(dir: &Path) -> Option<&'static str> {
    if dir.join("global.json").is_file() {
        return Some("global.json");
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut extensions: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            path.is_file()
                .then(|| path.extension()?.to_str().map(str::to_string))
                .flatten()
        })
        .collect();
    extensions.sort();
    if extensions.iter().any(|extension| extension == "slnx") {
        Some("slnx")
    } else if extensions.iter().any(|extension| extension == "sln") {
        Some("sln")
    } else {
        None
    }
}

fn nested_dotnet_web_project(dir: &Path, depth: usize, remaining: &mut usize) -> bool {
    let mut found = Vec::new();
    collect_dotnet_web_projects(dir, depth, remaining, true, &mut found);
    !found.is_empty()
}

/// Every web-SDK project file under a root, bounded like detection. Boot
/// recipe inference uses the full list: one project is a recipe, several are
/// a question the user answers.
pub(crate) fn dotnet_web_project_paths(dir: &Path) -> Vec<PathBuf> {
    let mut remaining = MAX_DOTNET_PROJECT_SCAN;
    let mut found = Vec::new();
    collect_dotnet_web_projects(dir, 0, &mut remaining, false, &mut found);
    found.sort();
    found
}

fn collect_dotnet_web_projects(
    dir: &Path,
    depth: usize,
    remaining: &mut usize,
    first_only: bool,
    found: &mut Vec<PathBuf>,
) {
    if depth > MAX_DOTNET_PROJECT_DEPTH || *remaining == 0 || (first_only && !found.is_empty()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            if path.is_dir() {
                return path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        !name.starts_with('.') && !DOTNET_SKIP_DIRS.contains(&name)
                    });
            }
            path.extension().and_then(|extension| extension.to_str()) == Some("csproj")
        })
        .collect();
    candidates.sort();
    for path in candidates {
        if *remaining == 0 || (first_only && !found.is_empty()) {
            return;
        }
        *remaining -= 1;
        if path.is_dir() {
            collect_dotnet_web_projects(&path, depth + 1, remaining, first_only, found);
        } else if is_dotnet_web_project(&path) {
            found.push(path);
        }
    }
}

/// Whether a solution root groups nested web projects but is not one itself.
pub(crate) fn is_dotnet_aggregator(dir: &Path) -> bool {
    dotnet_root_marker(dir).is_some() && !direct_dotnet_web_project(dir) && {
        let mut remaining = MAX_DOTNET_PROJECT_SCAN;
        nested_dotnet_web_project(dir, 0, &mut remaining)
    }
}

/// frontend framework, so web projects keep their web init.
pub fn detect_backend_framework(dir: &Path) -> Option<BackendFramework> {
    if let Some(name) = detect_cargo_framework(dir) {
        return Some(rust_framework(name));
    }
    if let Some(found) = node_project(dir) {
        return Some(found);
    }
    if let Some(found) = python_backend(dir) {
        return Some(found);
    }
    if let Some(found) = java_backend(dir) {
        return Some(found);
    }
    if let Some(gemfile) = manifest(dir, "Gemfile") {
        // A large Rails app often pins the components rather than the
        // metagem, and Rails itself uses `gemspec`. Requiring a literal
        // `gem "rails"` missed Discourse and rails/rails, both of which have a
        // config/routes.rb this reads perfectly once detection lets it.
        let rails = [
            "\"rails\"",
            "'rails'",
            "railties",
            "actionpack",
            "activerecord",
        ];
        if rails.iter().any(|needle| gemfile.contains(needle))
            || dir.join("config/routes.rb").exists()
        {
            return Some(framework(
                "rails",
                "Gemfile",
                "generate an OpenAPI schema with rswag (writes swagger/v1/swagger.yaml), then \
                 `reproit init swagger/v1/swagger.yaml`",
                "config.middleware.use ReproitBackendRb::Middleware, capture: capture",
            ));
        }
        if gemfile.contains("\"sinatra\"") || gemfile.contains("'sinatra'") {
            return Some(framework(
                "sinatra",
                "Gemfile",
                "hand-write openapi.yaml for the routes you serve, then `reproit init \
                 openapi.yaml`",
                "use ReproitBackendRb::Middleware, capture: capture",
            ));
        }
    }
    if let Some(composer) = manifest(dir, "composer.json") {
        let php = if composer.contains("laravel/framework") {
            Some((
                "laravel",
                "generate an OpenAPI schema with l5-swagger (writes \
                 storage/api-docs/api-docs.json), then `reproit init <that file or served url>`",
            ))
        } else if composer.contains("symfony/") {
            Some((
                "symfony",
                "add NelmioApiDocBundle (serves /api/doc.json), then `reproit init \
                 http://localhost:8000/api/doc.json`",
            ))
        } else {
            None
        };
        if let Some((name, hint)) = php {
            return Some(framework(
                name,
                "composer.json",
                hint,
                "$app->add(new ReproitMiddleware($capture))",
            ));
        }
    }
    if let Some(found) = dotnet_backend(dir) {
        return Some(found);
    }
    go_backend(dir)
}

pub(super) fn manifest(dir: &Path, name: &str) -> Option<String> {
    let path = dir.join(name);
    let small = std::fs::metadata(&path)
        .ok()
        .is_some_and(|meta| meta.is_file() && meta.len() <= MAX_MANIFEST_BYTES);
    if !small {
        return None;
    }
    std::fs::read_to_string(&path).ok()
}

/// Bound on workspace members inspected, so a large monorepo cannot turn
/// detection into a full-tree walk.
const MAX_WORKSPACE_MEMBERS: usize = 64;

/// The Rust framework for this directory, following a workspace root into its
/// members. A Cargo workspace root declares `[workspace] members = [...]` and
/// usually has no dependencies of its own, so reading only the root manifest
/// reported "no backend" for every workspace-layout service: the common shape
/// for exactly the Rust projects that most need source derivation.
fn detect_cargo_framework(dir: &Path) -> Option<&'static str> {
    let root = manifest(dir, "Cargo.toml")?;
    if let Some(name) = cargo_framework(&root) {
        return Some(name);
    }
    for member in workspace_members(&root, dir) {
        if let Some(name) = manifest(&member, "Cargo.toml")
            .as_deref()
            .and_then(cargo_framework)
        {
            return Some(name);
        }
    }
    None
}

/// Member directories declared by a workspace root, with a single trailing `*`
/// expanded one level. Deterministic (sorted) and bounded.
pub(super) fn workspace_members(cargo: &str, root: &Path) -> Vec<PathBuf> {
    let Some(list) = workspace_member_list(cargo) else {
        return Vec::new();
    };
    let mut members = Vec::new();
    for entry in list {
        if members.len() >= MAX_WORKSPACE_MEMBERS {
            break;
        }
        match entry.strip_suffix("/*") {
            Some(parent) => {
                let Ok(children) = std::fs::read_dir(root.join(parent)) else {
                    continue;
                };
                let mut paths: Vec<PathBuf> = children
                    .filter_map(Result::ok)
                    .map(|child| child.path())
                    .filter(|path| path.is_dir())
                    .collect();
                paths.sort();
                members.extend(paths.into_iter().take(MAX_WORKSPACE_MEMBERS));
            }
            None => members.push(root.join(entry)),
        }
    }
    members.truncate(MAX_WORKSPACE_MEMBERS);
    members
}

/// The raw `members = [...]` strings under `[workspace]`. Hand-scanned rather
/// than fully parsed: detection only needs the paths, and adding a TOML parser
/// for one array is not worth the dependency.
fn workspace_member_list(cargo: &str) -> Option<Vec<String>> {
    let start = cargo.find("[workspace]")?;
    let section = &cargo[start..];
    let members = section.find("members")?;
    let open = section[members..].find('[')? + members;
    let close = section[open..].find(']')? + open;
    Some(
        section[open + 1..close]
            .split(',')
            .map(|entry| entry.trim().trim_matches(['"', '\'']).to_string())
            .filter(|entry| !entry.is_empty())
            .collect(),
    )
}

pub(super) fn cargo_framework(cargo: &str) -> Option<&'static str> {
    ["axum", "actix-web", "rocket", "warp"]
        .into_iter()
        .find(|name| declares_dependency(cargo, name))
}

/// Whether a Cargo manifest declares a dependency, in any of the spellings.
///
/// `actix-web = "4"`, `actix-web = { workspace = true }` and
/// `actix-web.workspace = true` all declare it. Matching only the first two
/// missed 57 of 72 services in one real workspace, because the dotted-key form
/// is what a workspace member idiomatically writes.
fn declares_dependency(cargo: &str, name: &str) -> bool {
    cargo.lines().any(|line| {
        let line = line.trim();
        if line == format!("[dependencies.{name}]") {
            return true;
        }
        let Some(rest) = line.strip_prefix(name) else {
            return false;
        };
        let rest = rest.trim_start();
        rest.starts_with('=') || rest.starts_with('.')
    })
}

/// Conventional entry points for a Node service. Bounded and named rather than
/// walked: detection reads manifests, and a tree walk here would duplicate the
/// route reader's job at a fraction of its care.
const NODE_ENTRY_FILES: [&str; 8] = [
    "server.js",
    "server.mjs",
    "index.js",
    "index.mjs",
    "app.js",
    "src/server.js",
    "src/index.js",
    "src/app.js",
];

/// The Node framework of a project directory, package.json or not.
///
/// `http.createServer` is the node analogue of Go's `http.ListenAndServe`: it
/// is stdlib, so it is invisible in package.json, and it is the most common
/// server in the wild that no framework name covers. Leaving it unrecognised
/// is what let a backend-only repo fall through to the web scaffold.
fn node_project(dir: &Path) -> Option<BackendFramework> {
    if let Some(found) = manifest(dir, "package.json").and_then(|pkg| node_backend(&pkg)) {
        return Some(found);
    }
    // A frontend repo may ship a custom SSR server; its package.json says so,
    // and `node_backend` already bailed on it, so do not re-adopt it here.
    let frontend = manifest(dir, "package.json").is_some_and(|pkg| declares_frontend(&pkg));
    if frontend {
        return None;
    }
    let entry = NODE_ENTRY_FILES.into_iter().find(|name| {
        manifest(dir, name).is_some_and(|source| {
            source.contains("http.createServer") || source.contains("https.createServer")
        })
    })?;
    let mut found = node_framework("node:http");
    // Name the file the evidence actually came from: with no package.json
    // there is no manifest to point at, and pointing at one anyway is a lie
    // the user cannot check.
    found.manifest = if dir.join("package.json").is_file() {
        "package.json"
    } else {
        entry
    };
    Some(found)
}

/// Whether a package.json declares a UI framework. Shared by the framework bail
/// and the raw-server fallback so both read the same evidence.
fn declares_frontend(pkg: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(pkg) else {
        return false;
    };
    ["react", "vue", "svelte", "next", "@angular/core"]
        .iter()
        .any(|name| {
            ["dependencies", "devDependencies"].iter().any(|section| {
                parsed
                    .get(section)
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|deps| deps.contains_key(*name))
            })
        })
}

fn node_backend(pkg: &str) -> Option<BackendFramework> {
    let parsed: serde_json::Value = serde_json::from_str(pkg).ok()?;
    let has_dep = |name: &str| {
        ["dependencies", "devDependencies"].iter().any(|section| {
            parsed
                .get(section)
                .and_then(serde_json::Value::as_object)
                .is_some_and(|deps| deps.contains_key(name))
        })
    };
    // Checked BEFORE the frontend bail. A Nest server that renders its
    // transactional email with @react-email declares `react`, and bailing on
    // that made the largest real Nest backend tested invisible: 42 controllers
    // and 281 route decorators, reported as "no backend framework".
    //
    // NestJS also runs ON express and declares both; its routes are decorators
    // on a controller class rather than express-style registrations, so
    // labelling it express pointed the guidance at the wrong schema generator.
    if has_dep("@nestjs/core") || has_dep("@nestjs/common") {
        return Some(node_framework("nestjs"));
    }
    if declares_frontend(pkg) {
        return None;
    }
    for name in ["express", "fastify", "koa", "@hapi/hapi", "hapi"] {
        if has_dep(name) {
            let label: &'static str = match name {
                "express" => "express",
                "fastify" => "fastify",
                "koa" => "koa",
                _ => "hapi",
            };
            return Some(node_framework(label));
        }
    }
    None
}

fn python_backend(dir: &Path) -> Option<BackendFramework> {
    for name in ["pyproject.toml", "requirements.txt"] {
        let Some(content) = manifest(dir, name) else {
            continue;
        };
        let content = content.to_ascii_lowercase();
        let manifest_name: &'static str = if name == "pyproject.toml" {
            "pyproject.toml"
        } else {
            "requirements.txt"
        };
        for pkg in ["fastapi", "django", "flask"] {
            if content.contains(pkg) {
                return Some(python_framework(pkg, manifest_name));
            }
        }
    }
    None
}

fn java_backend(dir: &Path) -> Option<BackendFramework> {
    let (content, name) = ["pom.xml", "build.gradle", "build.gradle.kts"]
        .into_iter()
        .find_map(|file| Some((manifest(dir, file)?, file)))?;
    let spring = content.contains("spring");
    Some(framework(
        if spring { "spring" } else { "java" },
        match name {
            "pom.xml" => "pom.xml",
            "build.gradle" => "build.gradle",
            _ => "build.gradle.kts",
        },
        if spring {
            "add springdoc-openapi (serves /v3/api-docs), then `reproit init \
             http://localhost:8080/v3/api-docs`"
        } else {
            "serve or export an OpenAPI schema (springdoc for Spring, or hand-write \
             openapi.yaml), then `reproit init <schema url or file>`"
        },
        "new FilterRegistrationBean<>(new ReproitFilter(capture))  // register on /*",
    ))
}

fn go_backend(dir: &Path) -> Option<BackendFramework> {
    if let Some(go_mod) = manifest(dir, "go.mod") {
        for (needle, name) in [
            ("github.com/gin-gonic/gin", "gin"),
            ("github.com/labstack/echo", "echo"),
            ("github.com/gofiber/fiber", "fiber"),
            ("github.com/go-chi/chi", "chi"),
            ("github.com/gorilla/mux", "gorilla/mux"),
        ] {
            if go_mod.contains(needle) {
                return Some(go_framework(name));
            }
        }
    }
    // net/http is stdlib and invisible in go.mod: a root main.go that calls
    // http.ListenAndServe is the marker for a plain Go HTTP server.
    let main_go = manifest(dir, "main.go")?;
    main_go
        .contains("http.ListenAndServe")
        .then(|| go_framework("net/http"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> std::path::PathBuf {
        // A wall-clock name alone collides when two parallel tests hit the same
        // clock tick (macOS resolves to microseconds, and the pid is shared
        // across threads), so two tests share one directory and race each
        // other's writes and cleanup. The process-wide counter makes each call
        // unique regardless of clock resolution. Same fix as
        // `workflows::backend_learn::tests::project`.
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "reproit-backend-detect-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, content).unwrap();
        }
        dir
    }

    #[test]
    fn manifest_detection_table_covers_every_ecosystem() {
        let cases: &[(&[(&str, &str)], &str)] = &[
            (
                &[("Cargo.toml", "[dependencies]\naxum = \"0.8\"\n")],
                "axum",
            ),
            (
                &[("Cargo.toml", "[dependencies]\nactix-web = \"4\"\n")],
                "actix-web",
            ),
            (
                &[(
                    "Cargo.toml",
                    "[dependencies]\nrocket = { version = \"0.5\" }\n",
                )],
                "rocket",
            ),
            (
                &[("Cargo.toml", "[dependencies]\nwarp = \"0.3\"\n")],
                "warp",
            ),
            (
                &[("package.json", r#"{"dependencies":{"express":"^4"}}"#)],
                "express",
            ),
            (
                &[("package.json", r#"{"dependencies":{"fastify":"^5"}}"#)],
                "fastify",
            ),
            (
                &[("package.json", r#"{"dependencies":{"koa":"^2"}}"#)],
                "koa",
            ),
            (
                &[("package.json", r#"{"dependencies":{"@hapi/hapi":"^21"}}"#)],
                "hapi",
            ),
            (
                &[(
                    "pyproject.toml",
                    "[project]\ndependencies = [\"fastapi\"]\n",
                )],
                "fastapi",
            ),
            (&[("requirements.txt", "Flask==3.0\n")], "flask"),
            (&[("requirements.txt", "django-ninja\n")], "django"),
            (
                &[(
                    "pom.xml",
                    "<project><artifactId>spring-boot</artifactId></project>",
                )],
                "spring",
            ),
            (&[("build.gradle", "plugins { id 'java' }\n")], "java"),
            (&[("Gemfile", "gem \"rails\"\n")], "rails"),
            (&[("Gemfile", "gem 'sinatra'\n")], "sinatra"),
            (
                &[(
                    "composer.json",
                    r#"{"require":{"laravel/framework":"^11"}}"#,
                )],
                "laravel",
            ),
            (
                &[(
                    "composer.json",
                    r#"{"require":{"symfony/http-kernel":"^7"}}"#,
                )],
                "symfony",
            ),
            (
                &[(
                    "go.mod",
                    "module x\nrequire github.com/gin-gonic/gin v1.10.0\n",
                )],
                "gin",
            ),
            (
                &[(
                    "go.mod",
                    "module x\nrequire github.com/labstack/echo/v4 v4.12.0\n",
                )],
                "echo",
            ),
            (
                &[(
                    "go.mod",
                    "module x\nrequire github.com/gofiber/fiber/v2 v2.52.0\n",
                )],
                "fiber",
            ),
            (
                &[(
                    "go.mod",
                    "module x\nrequire github.com/go-chi/chi/v5 v5.1.0\n",
                )],
                "chi",
            ),
            (
                &[
                    ("go.mod", "module x\n"),
                    (
                        "main.go",
                        "func main() { http.ListenAndServe(\":8080\", nil) }\n",
                    ),
                ],
                "net/http",
            ),
            (
                &[
                    ("package.json", r#"{"name":"svc"}"#),
                    (
                        "server.js",
                        "const http = require('http');\n\
                         http.createServer((req, res) => res.end('ok')).listen(3000);\n",
                    ),
                ],
                "node:http",
            ),
            (
                &[(
                    "src/index.js",
                    "import http from 'node:http';\n\
                     http.createServer((req, res) => res.end('ok')).listen(3000);\n",
                )],
                "node:http",
            ),
        ];
        for (files, expected) in cases {
            let dir = project(files);
            let detected = detect_backend_framework(&dir);
            assert_eq!(
                detected.as_ref().map(|found| found.name),
                Some(*expected),
                "files {files:?}"
            );
            let found = detected.unwrap();
            assert!(!found.schema_hint.is_empty());
            assert!(!found.adapter_snippet.is_empty());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn frontend_and_unknown_projects_are_not_backends() {
        for files in [
            vec![(
                "package.json",
                r#"{"dependencies":{"express":"^4","react":"^19"}}"#,
            )],
            vec![("package.json", r#"{"dependencies":{"lodash":"^4"}}"#)],
            vec![("Cargo.toml", "[dependencies]\nserde = \"1\"\n")],
            vec![("go.mod", "module x\n")],
            vec![("main.txt", "not a manifest\n")],
        ] {
            let dir = project(&files);
            assert!(detect_backend_framework(&dir).is_none(), "files {files:?}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_frontend_repo_with_its_own_server_stays_a_frontend() {
        // Next-style SSR: a custom server.js standing up node:http next to a
        // React dependency. The UI declaration is the stronger evidence, and
        // adopting the file as a backend would hijack the web init.
        let dir = project(&[
            ("package.json", r#"{"dependencies":{"react":"^19"}}"#),
            (
                "server.js",
                "const http = require('http');\n\
                 http.createServer(handler).listen(3000);\n",
            ),
        ]);
        assert!(detect_backend_framework(&dir).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_raw_node_server_names_the_file_it_was_read_from() {
        // With no package.json there is no manifest to point at, so the
        // evidence line names the source file the user can go look at.
        let dir = project(&[(
            "server.js",
            "const http = require('http');\n\
             http.createServer((req, res) => res.end('ok')).listen(3000);\n",
        )]);
        let found = detect_backend_framework(&dir).expect("a raw node server is a backend");
        assert_eq!(found.name, "node:http");
        assert_eq!(found.manifest, "server.js");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cargo_dependency_table_headers_detect_the_framework() {
        let dir = project(&[(
            "Cargo.toml",
            "[package]\nname = \"serialization\"\nversion = \"0.1.0\"\n\
             [dependencies.rocket]\nversion = \"0.5\"\n",
        )]);
        let detected = detect_backend_framework(&dir);
        assert_eq!(
            detected.as_ref().map(|framework| framework.name),
            Some("rocket")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gorilla_mux_module_detects_the_existing_go_route_reader() {
        let dir = project(&[(
            "go.mod",
            "module example.test/service\n\
             require github.com/gorilla/mux v1.8.1\n",
        )]);
        let detected = detect_backend_framework(&dir);
        assert_eq!(
            detected.as_ref().map(|framework| framework.name),
            Some("gorilla/mux")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dotnet_root_markers_find_a_nested_web_project() {
        for marker in ["Repo.sln", "Repo.slnx", "global.json"] {
            let dir = project(&[
                (marker, "{}"),
                (
                    "src/Web/Web.csproj",
                    r#"<Project Sdk="Microsoft.NET.Sdk.Web"></Project>"#,
                ),
                (
                    "src/Web/Program.cs",
                    r#"app.MapGet("/health", () => "ok");"#,
                ),
            ]);
            let detected = detect_backend_framework(&dir);
            assert_eq!(
                detected.as_ref().map(|framework| framework.name),
                Some("aspnet"),
                "marker {marker}"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn razor_and_blazor_sdk_projects_are_dotnet_web_projects() {
        for sdk in [
            "Microsoft.NET.Sdk.Razor",
            "Microsoft.NET.Sdk.BlazorWebAssembly",
        ] {
            let project_file = format!(r#"<Project Sdk="{sdk}"></Project>"#);
            let dir = project(&[("App.csproj", &project_file)]);
            let detected = detect_backend_framework(&dir);
            assert_eq!(
                detected.as_ref().map(|framework| framework.name),
                Some("aspnet"),
                "SDK {sdk}"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
