use super::parse_str;
use std::path::PathBuf;

#[test]
fn accepts_known_principals_and_exact_outcomes() {
    let yaml = r#"
app: { platform: web, webRunnerDir: ./runner, url: "https://app.test" }
devices: { namePrefix: route-access }
journeys: { driver: noop, doneMarkers: [done] }
auth:
  accounts:
    - name: member
      strategy: session
      storageRef: member-session
      validate: { route: /app }
routeAccess:
  - route: /login
    access:
      anonymous: allow
      member: { redirect: /app }
  - route: /app
    access:
      anonymous: { redirect: /login }
      member: allow
"#;
    let loaded = parse_str(yaml, PathBuf::from("/tmp/route-access")).unwrap();
    assert_eq!(loaded.config.route_access.len(), 2);
}

#[test]
fn rejects_inferred_principals_and_ambiguous_urls() {
    let base = r#"
app: { platform: web, webRunnerDir: ./runner, url: "https://app.test" }
devices: { namePrefix: route-access }
journeys: { driver: noop, doneMarkers: [done] }
routeAccess:
  - route: ROUTE
    access: { guessed-admin: allow }
"#;
    let unknown = parse_str(
        &base.replace("ROUTE", "/admin"),
        PathBuf::from("/tmp/route-access"),
    )
    .err()
    .expect("unknown principal must fail")
    .to_string();
    assert!(unknown.contains("not anonymous or an auth account"));

    let query = parse_str(
        &base
            .replace("ROUTE", "/admin?tenant=one")
            .replace("guessed-admin", "anonymous"),
        PathBuf::from("/tmp/route-access"),
    )
    .err()
    .expect("query route must fail")
    .to_string();
    assert!(query.contains("without query or fragment"));
}
