//! Boot recipe inference: the build and exec commands `reproit init` may
//! boot for live enrichment and record as `backend.exec` once proven.
//!
//! One rule governs every row: a recipe is only ever a candidate. It becomes
//! configuration solely after `boot.rs` starts it and a derived route
//! answers. When several commands are equally valid the inference abstains
//! and names the choices, because a guessed winner that builds but serves
//! the wrong service is worse than asking.
//!
//! Recorded exec strings embed `${PORT:-...}` where a server takes its port
//! as an argument; every consumer (this boot, hermetic replay, re-record)
//! runs the command through `sh -c` with `PORT` set, so the same string
//! works unchanged in all of them.

use std::path::{Path, PathBuf};

use crate::adapters::project_scaffold::{backend_detect, cargo_bins};

/// Bounds for the Python entry-point scan. A repo where the app object is
/// deeper or the tree is larger falls back to abstention, never to a walk.
const MAX_PY_FILES: usize = 400;
const MAX_PY_DEPTH: usize = 3;
const MAX_PY_FILE_BYTES: u64 = 256 * 1024;
const PY_SKIP_DIRS: [&str; 8] = [
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "__pycache__",
    "tests",
    "test",
    "migrations",
];

/// A boot candidate: what to run, and why this command was selected.
pub(crate) struct BootRecipe {
    /// Bounded pre-launch step for compiled ecosystems (e.g. `cargo build`).
    pub(crate) build: Option<String>,
    /// The durable command: recorded as `backend.exec` once proven, and used
    /// by hermetic replay. Run via `sh -c` with `PORT` in the environment.
    pub(crate) exec: String,
    /// The command this run actually spawns. Identical to `exec` except for
    /// Node, where the raw script body runs with node_modules/.bin on PATH
    /// (matching what `npm run` does) while `npm run <name>` is recorded.
    pub(crate) boot: String,
    /// Where the recipe came from, for say lines and the config comment.
    pub(crate) evidence: String,
}

/// What inference concluded. `Ambiguous` is a first-class outcome: init says
/// the candidates and the `--exec` rerun instead of guessing.
pub(crate) enum Inference {
    Recipe(BootRecipe),
    Ambiguous {
        candidates: Vec<String>,
        hint: String,
    },
    None,
}

/// Infer the boot recipe for a detected framework. Reads manifests and
/// bounded source only; runs nothing.
pub(crate) fn infer(root: &Path, framework: &str) -> Inference {
    match framework {
        "express" | "fastify" | "koa" | "hapi" | "nestjs" | "node:http" => node_recipe(root),
        "axum" | "actix-web" | "rocket" | "warp" => rust_recipe(root),
        "fastapi" => python_app_recipe(root, "FastAPI", "uvicorn", 8000),
        "flask" => python_app_recipe(root, "Flask", "flask", 5000),
        "django" => django_recipe(root),
        "gin" | "echo" | "fiber" | "chi" | "gorilla/mux" | "net/http" => go_recipe(root),
        "aspnet" => dotnet_recipe(root),
        "spring" => spring_recipe(root),
        "rails" => rails_recipe(root),
        "laravel" => php_recipe(root),
        _ => Inference::None,
    }
}

/// Go: `go run .` when the module root is the main package, else the single
/// `cmd/<name>` main. Several mains are a question, not a guess.
fn go_recipe(root: &Path) -> Inference {
    let root_main = std::fs::read_to_string(root.join("main.go"))
        .is_ok_and(|source| source.contains("func main"));
    if root_main {
        return recipe(
            Some("go build ./...".to_string()),
            "go run .".to_string(),
            "the main package at the module root".to_string(),
        );
    }
    let mut mains = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("cmd")) {
        for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
            if path.is_dir() && path.join("main.go").is_file() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    mains.push(format!("./cmd/{name}"));
                }
            }
        }
    }
    mains.sort();
    match mains.as_slice() {
        [] => Inference::None,
        [main] => recipe(
            Some("go build ./...".to_string()),
            format!("go run {main}"),
            format!("the only main package ({main})"),
        ),
        _ => Inference::Ambiguous {
            candidates: mains.iter().map(|main| format!("go run {main}")).collect(),
            hint: "this module has several main packages under cmd/".to_string(),
        },
    }
}

/// .NET: the web-SDK project detection already found. ASP.NET Core reads
/// `ASPNETCORE_URLS`, so the exec pins the listen address to the port.
fn dotnet_recipe(root: &Path) -> Inference {
    let projects = backend_detect::dotnet_web_project_paths(root);
    let command = |project: &Path| {
        let relative = project.strip_prefix(root).unwrap_or(project).display();
        format!("ASPNETCORE_URLS=http://127.0.0.1:${{PORT:-5000}} dotnet run --project {relative}")
    };
    match projects.as_slice() {
        [] => Inference::None,
        [project] => recipe(
            None,
            command(project),
            format!(
                "the only web project ({})",
                project.strip_prefix(root).unwrap_or(project).display()
            ),
        ),
        _ => Inference::Ambiguous {
            candidates: projects.iter().map(|project| command(project)).collect(),
            hint: "this solution has several web projects".to_string(),
        },
    }
}

/// Spring Boot through the repo's own wrapper when present, so the exec runs
/// on the toolchain the repo pins.
fn spring_recipe(root: &Path) -> Inference {
    let (exec, evidence) = if root.join("gradlew").is_file() {
        ("./gradlew bootRun", "the Gradle wrapper")
    } else if root.join("mvnw").is_file() {
        ("./mvnw spring-boot:run", "the Maven wrapper")
    } else if root.join("pom.xml").is_file() {
        ("mvn spring-boot:run", "pom.xml")
    } else {
        ("gradle bootRun", "the Gradle build file")
    };
    // bootRun/spring-boot:run take the port from the application config, not
    // the environment; the silent-port fallback adopts the bound port.
    recipe(None, exec.to_string(), evidence.to_string())
}

fn rails_recipe(root: &Path) -> Inference {
    let exec = if root.join("bin/rails").is_file() {
        "bin/rails server -b 127.0.0.1 -p ${PORT:-3000}"
    } else {
        "bundle exec rails server -b 127.0.0.1 -p ${PORT:-3000}"
    };
    recipe(
        None,
        exec.to_string(),
        "the Rails server command".to_string(),
    )
}

fn php_recipe(root: &Path) -> Inference {
    if !root.join("artisan").is_file() {
        return Inference::None;
    }
    recipe(
        None,
        "php artisan serve --host 127.0.0.1 --port ${PORT:-8000}".to_string(),
        "artisan".to_string(),
    )
}

/// The unambiguous recipe for this project, framework detection included.
/// None when nothing is inferable or several candidates tie.
pub(crate) fn inferred(root: &Path) -> Option<BootRecipe> {
    let framework = backend_detect::detect_backend_framework(root)?;
    match infer(root, framework.name) {
        Inference::Recipe(recipe) => Some(recipe),
        Inference::Ambiguous { .. } | Inference::None => None,
    }
}

/// The recorded exec suggestion for a project, proof pending. The scaffold
/// writes this as a commented suggestion when init could not prove a boot
/// this run; `None` when nothing is inferable or the choice is ambiguous.
pub(crate) fn suggested_exec(root: &Path) -> Option<String> {
    inferred(root).map(|recipe| recipe.exec)
}

fn recipe(build: Option<String>, exec: String, evidence: String) -> Inference {
    Inference::Recipe(BootRecipe {
        build,
        boot: exec.clone(),
        exec,
        evidence,
    })
}

/// The package.json script bare init may boot: `start` first, then `dev`.
fn node_recipe(root: &Path) -> Inference {
    let Ok(raw) = std::fs::read_to_string(root.join("package.json")) else {
        return Inference::None;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Inference::None;
    };
    let Some(scripts) = parsed.get("scripts").and_then(serde_json::Value::as_object) else {
        return Inference::None;
    };
    for name in ["start", "dev"] {
        if let Some(command) = scripts.get(name).and_then(serde_json::Value::as_str) {
            if !command.trim().is_empty() {
                return Inference::Recipe(BootRecipe {
                    build: None,
                    exec: format!("npm run {name}"),
                    boot: command.to_string(),
                    evidence: format!("the package.json `{name}` script"),
                });
            }
        }
    }
    Inference::None
}

/// Rust: the server binary, enumerated from the packages that declare the
/// framework. One binary is a recipe; several are a question.
fn rust_recipe(root: &Path) -> Inference {
    let bins = cargo_bins::cargo_server_bins(root);
    let locked = if root.join("Cargo.lock").is_file() {
        " --locked"
    } else {
        ""
    };
    match bins.as_slice() {
        [] => Inference::None,
        [bin] => recipe(
            Some(format!("cargo build{locked} --bin {bin}")),
            format!("cargo run{locked} --bin {bin}"),
            format!("the only server binary (`{bin}`) in this workspace"),
        ),
        _ => Inference::Ambiguous {
            candidates: bins
                .iter()
                .map(|bin| format!("cargo run{locked} --bin {bin}"))
                .collect(),
            hint: "this workspace has several server binaries".to_string(),
        },
    }
}

/// The command that runs `tool` inside the project's own environment: the
/// lockfile's runner, else the repo-local `.venv` binary when it exists,
/// else the bare tool. A bare `uvicorn` against a repo whose dependencies
/// live in `.venv` exits 127 before serving anything.
fn python_tool(root: &Path, tool: &str) -> String {
    if root.join("uv.lock").is_file() {
        return format!("uv run {tool}");
    }
    if root.join("poetry.lock").is_file() {
        return format!("poetry run {tool}");
    }
    if root.join(".venv/bin").join(tool).is_file() {
        return format!(".venv/bin/{tool}");
    }
    tool.to_string()
}

fn django_recipe(root: &Path) -> Inference {
    if !root.join("manage.py").is_file() {
        return Inference::None;
    }
    let python = python_tool(root, "python");
    recipe(
        None,
        format!("{python} manage.py runserver 127.0.0.1:${{PORT:-8000}}"),
        "manage.py".to_string(),
    )
}

/// FastAPI / Flask: find the module-level `<var> = Framework(` assignment
/// that names the app object a server command needs.
fn python_app_recipe(root: &Path, class: &str, launcher: &str, port: u16) -> Inference {
    let mut apps = python_app_objects(root, class);
    if apps.len() > 1 {
        // Several files declare an app object. Conventional entry names win
        // when exactly one candidate carries one; otherwise abstain.
        let conventional: Vec<usize> = apps
            .iter()
            .enumerate()
            .filter(|(_, (module, _))| {
                let last = module.rsplit('.').next().unwrap_or(module);
                matches!(last, "main" | "app" | "wsgi" | "asgi")
            })
            .map(|(index, _)| index)
            .collect();
        if let [only] = conventional.as_slice() {
            apps = vec![apps[*only].clone()];
        }
    }
    let tool = python_tool(root, launcher);
    // `FLASK_APP=... flask run` works on every Flask; the `--app` flag only
    // exists from Flask 2.2, and an old pin exits 2 on it before serving.
    let command = |module: &str, var: &str| match launcher {
        "flask" => {
            format!("FLASK_APP={module}:{var} {tool} run --host 127.0.0.1 --port ${{PORT:-{port}}}")
        }
        _ => format!("{tool} {module}:{var} --host 127.0.0.1 --port ${{PORT:-{port}}}"),
    };
    match apps.as_slice() {
        [] => Inference::None,
        [(module, var)] => recipe(
            None,
            command(module, var),
            format!(
                "the {class} app object `{var}` in {}",
                module.replace('.', "/") + ".py"
            ),
        ),
        _ => Inference::Ambiguous {
            candidates: apps
                .iter()
                .map(|(module, var)| command(module, var))
                .collect(),
            hint: format!("several modules declare a {class} app object"),
        },
    }
}

/// Bounded scan for `<var> = Class(` at module level. Returns (dotted module,
/// variable) pairs, sorted for determinism.
fn python_app_objects(root: &Path, class: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    collect_py_files(root, 0, &mut files);
    files.sort();
    let needle = format!("{class}(");
    let mut found = Vec::new();
    for path in files {
        let small = std::fs::metadata(&path)
            .ok()
            .is_some_and(|meta| meta.len() <= MAX_PY_FILE_BYTES);
        if !small {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in source.lines() {
            // Module level only: an indented assignment is inside a function
            // or class and is not importable as `module:var`.
            if line.starts_with(char::is_whitespace) {
                continue;
            }
            let Some((left, right)) = line.split_once('=') else {
                continue;
            };
            let var = left.trim();
            let is_identifier = !var.is_empty()
                && var
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
            if is_identifier && right.trim().starts_with(&needle) {
                if let Some(module) = dotted_module(root, &path) {
                    found.push((module, var.to_string()));
                }
                break;
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

fn collect_py_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_PY_DEPTH || out.len() >= MAX_PY_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    for path in paths {
        if out.len() >= MAX_PY_FILES {
            return;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !PY_SKIP_DIRS.contains(&name) {
                collect_py_files(&path, depth + 1, out);
            }
        } else if name.ends_with(".py") && name != "conftest.py" && !name.starts_with("test_") {
            out.push(path);
        }
    }
}

/// `app/main.py` -> `app.main`; a package `__init__.py` names its package.
fn dotted_module(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts: Vec<String> = relative
        .components()
        .filter_map(|part| part.as_os_str().to_str().map(str::to_string))
        .collect();
    let last = parts.pop()?;
    let stem = last.strip_suffix(".py")?;
    if stem != "__init__" {
        parts.push(stem.to_string());
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> std::path::PathBuf {
        // Unique per call for the same reason as the backend_detect helper:
        // a clock-only name collides across parallel tests.
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "reproit-boot-recipe-{}-{}",
            std::process::id(),
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

    fn expect_recipe(files: &[(&str, &str)], framework: &str) -> BootRecipe {
        let dir = project(files);
        let inference = infer(&dir, framework);
        std::fs::remove_dir_all(&dir).unwrap();
        match inference {
            Inference::Recipe(recipe) => recipe,
            Inference::Ambiguous { candidates, .. } => {
                panic!("expected a recipe for {framework}, got ambiguity: {candidates:?}")
            }
            Inference::None => panic!("expected a recipe for {framework}, got none"),
        }
    }

    #[test]
    fn recipe_table_covers_every_ecosystem() {
        let node = expect_recipe(
            &[(
                "package.json",
                r#"{"dependencies":{"express":"^4"},"scripts":{"start":"node server.js"}}"#,
            )],
            "express",
        );
        assert_eq!(node.exec, "npm run start");
        assert_eq!(node.boot, "node server.js");
        assert!(node.build.is_none());

        let rust = expect_recipe(
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"api\"\n[dependencies]\naxum = \"0.8\"\n",
                ),
                ("src/main.rs", "fn main() {}\n"),
                ("Cargo.lock", ""),
            ],
            "axum",
        );
        assert_eq!(rust.exec, "cargo run --locked --bin api");
        assert_eq!(
            rust.build.as_deref(),
            Some("cargo build --locked --bin api")
        );

        let fastapi = expect_recipe(
            &[
                (
                    "pyproject.toml",
                    "[project]\ndependencies = [\"fastapi\"]\n",
                ),
                ("uv.lock", ""),
                ("app/__init__.py", ""),
                (
                    "app/main.py",
                    "from fastapi import FastAPI\napp = FastAPI()\n",
                ),
            ],
            "fastapi",
        );
        assert_eq!(
            fastapi.exec,
            "uv run uvicorn app.main:app --host 127.0.0.1 --port ${PORT:-8000}"
        );

        let django = expect_recipe(
            &[
                ("requirements.txt", "django\n"),
                ("manage.py", "#!/usr/bin/env python\n"),
            ],
            "django",
        );
        assert_eq!(
            django.exec,
            "python manage.py runserver 127.0.0.1:${PORT:-8000}"
        );

        let flask = expect_recipe(
            &[
                ("requirements.txt", "Flask==3.0\n"),
                ("app.py", "from flask import Flask\napp = Flask(__name__)\n"),
            ],
            "flask",
        );
        assert_eq!(
            flask.exec,
            "FLASK_APP=app:app flask run --host 127.0.0.1 --port ${PORT:-5000}"
        );

        let go_root = expect_recipe(
            &[
                (
                    "go.mod",
                    "module x\nrequire github.com/gin-gonic/gin v1.10.0\n",
                ),
                ("main.go", "package main\nfunc main() {}\n"),
            ],
            "gin",
        );
        assert_eq!(go_root.exec, "go run .");

        let go_cmd = expect_recipe(
            &[
                (
                    "go.mod",
                    "module x\nrequire github.com/go-chi/chi/v5 v5.1.0\n",
                ),
                ("cmd/api/main.go", "package main\nfunc main() {}\n"),
            ],
            "chi",
        );
        assert_eq!(go_cmd.exec, "go run ./cmd/api");
        assert_eq!(go_cmd.build.as_deref(), Some("go build ./..."));

        let dotnet = expect_recipe(
            &[
                ("App.sln", ""),
                (
                    "src/Web/Web.csproj",
                    r#"<Project Sdk="Microsoft.NET.Sdk.Web"></Project>"#,
                ),
            ],
            "aspnet",
        );
        assert_eq!(
            dotnet.exec,
            "ASPNETCORE_URLS=http://127.0.0.1:${PORT:-5000} dotnet run --project src/Web/Web.csproj"
        );

        let spring = expect_recipe(&[("gradlew", ""), ("build.gradle", "")], "spring");
        assert_eq!(spring.exec, "./gradlew bootRun");

        let rails = expect_recipe(&[("Gemfile", "gem \"rails\"\n")], "rails");
        assert_eq!(
            rails.exec,
            "bundle exec rails server -b 127.0.0.1 -p ${PORT:-3000}"
        );

        let laravel = expect_recipe(&[("artisan", "")], "laravel");
        assert_eq!(
            laravel.exec,
            "php artisan serve --host 127.0.0.1 --port ${PORT:-8000}"
        );
    }

    #[test]
    fn several_candidates_abstain_instead_of_guessing() {
        let dir = project(&[
            (
                "Cargo.toml",
                "[package]\nname = \"svc\"\n[dependencies]\naxum = \"0.8\"\n",
            ),
            ("src/main.rs", "fn main() {}\n"),
            ("src/bin/worker.rs", "fn main() {}\n"),
        ]);
        let inference = infer(&dir, "axum");
        assert!(suggested_exec(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
        let Inference::Ambiguous { candidates, .. } = inference else {
            panic!("two bins must be ambiguous");
        };
        assert_eq!(
            candidates,
            vec!["cargo run --bin svc", "cargo run --bin worker"]
        );

        let go = project(&[
            (
                "go.mod",
                "module x\nrequire github.com/gin-gonic/gin v1.10.0\n",
            ),
            ("cmd/api/main.go", "package main\nfunc main() {}\n"),
            ("cmd/worker/main.go", "package main\nfunc main() {}\n"),
        ]);
        let inference = infer(&go, "gin");
        std::fs::remove_dir_all(&go).unwrap();
        assert!(matches!(inference, Inference::Ambiguous { .. }));
    }

    #[test]
    fn a_conventional_python_entry_wins_over_a_secondary_app_object() {
        let dir = project(&[
            (
                "pyproject.toml",
                "[project]\ndependencies = [\"fastapi\"]\n",
            ),
            ("app/__init__.py", ""),
            (
                "app/main.py",
                "from fastapi import FastAPI\napp = FastAPI()\n",
            ),
            (
                "app/admin.py",
                "from fastapi import FastAPI\nadmin = FastAPI()\n",
            ),
        ]);
        let recipe = match infer(&dir, "fastapi") {
            Inference::Recipe(recipe) => recipe,
            _ => panic!("main.py must win"),
        };
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(recipe.exec.contains("app.main:app"));
    }

    #[test]
    fn module_level_scan_ignores_indented_assignments() {
        let dir = project(&[
            ("requirements.txt", "Flask==3.0\n"),
            (
                "factory.py",
                "def create():\n    app = Flask(__name__)\n    return app\n",
            ),
        ]);
        let inference = infer(&dir, "flask");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(inference, Inference::None));
    }
}
