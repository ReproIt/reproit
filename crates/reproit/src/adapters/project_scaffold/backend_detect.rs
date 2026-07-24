//! Backend framework detection from project manifests, plus the per-framework
//! schema guidance and adapter one-liner that `init` and `doctor` teach.
//! Detection is a hint for guidance only; it never creates configuration.

use std::path::Path;

/// Manifests are small; anything larger is not a manifest we should parse.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

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
        _ => NODE_EXPRESS_SNIPPET,
    };
    let hint = match name {
        "fastify" => {
            "register @fastify/swagger (serves /documentation/json), then `reproit init \
                      http://localhost:3000/documentation/json`"
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
/// frontend framework, so web projects keep their web init.
pub fn detect_backend_framework(dir: &Path) -> Option<BackendFramework> {
    if let Some(name) = manifest(dir, "Cargo.toml")
        .as_deref()
        .and_then(cargo_framework)
    {
        return Some(rust_framework(name));
    }
    if let Some(found) = manifest(dir, "package.json").and_then(|pkg| node_backend(&pkg)) {
        return Some(found);
    }
    if let Some(found) = python_backend(dir) {
        return Some(found);
    }
    if let Some(found) = java_backend(dir) {
        return Some(found);
    }
    if let Some(gemfile) = manifest(dir, "Gemfile") {
        if gemfile.contains("\"rails\"") || gemfile.contains("'rails'") {
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
    go_backend(dir)
}

fn manifest(dir: &Path, name: &str) -> Option<String> {
    let path = dir.join(name);
    let small = std::fs::metadata(&path)
        .ok()
        .is_some_and(|meta| meta.is_file() && meta.len() <= MAX_MANIFEST_BYTES);
    if !small {
        return None;
    }
    std::fs::read_to_string(&path).ok()
}

fn cargo_framework(cargo: &str) -> Option<&'static str> {
    ["axum", "actix-web", "rocket", "warp"]
        .into_iter()
        .find(|name| {
            cargo
                .lines()
                .any(|line| line.trim_start().starts_with(&format!("{name} ")))
                || cargo.contains(&format!("\n{name} ="))
                || cargo.contains(&format!("\n{name}="))
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
    let frontend = ["react", "vue", "svelte", "next", "@angular/core"];
    if frontend.iter().any(|name| has_dep(name)) {
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
        let dir = std::env::temp_dir().join(format!(
            "reproit-backend-detect-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
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
}
