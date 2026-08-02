//! Planted order-dependent test failure that fires only under CI-like
//! conditions, for the flaky-CI wedge on the Rust SDK.
//!
//! The first test runs ONLY on the CI legacy matrix (CI_LEGACY_MATRIX=1) and
//! leaks state into the shared config service: it switches the service to
//! its legacy response format, which returns the tax rate as a string. The
//! second test then computes a wrong total and fails. A plain local run
//! never takes the legacy branch, so the suite passes and the failure looks
//! unreproducible ("flaky"). The capsule spooled by the CI run carries the
//! recorded legacy response, so `reproit check <capsule> --exec "cargo test
//! ... -- --test-threads=1"` re-executes the exact failing run anywhere.
//!
//! Run with `--test-threads=1`: libtest then runs the tests sequentially in
//! name-sorted order, which the a_/b_ prefixes pin. The default parallel run
//! cannot guarantee the order this fixture plants; that constraint is the
//! named cargo-test deviation in the SDK README.

use reproit_backend::{ci, instrument};
use rs_flaky_ci_fixture::{config_url, order_total};

#[tokio::test]
async fn a_legacy_config_format_toggles() {
    ci::run("checkout", "legacy config format toggles", async {
        // CI-only: this is the state leak that makes the next test order
        // dependent. A local run never takes this branch.
        if std::env::var("CI_LEGACY_MATRIX").ok().as_deref() != Some("1") {
            return;
        }
        let client = reqwest::Client::new();
        let request = client
            .post(format!("{}/format/legacy", config_url()))
            .build()
            .expect("request");
        let response = instrument::http::send(&client, request)
            .await
            .expect("config service");
        assert_eq!(response.status, 204);
    })
    .await;
}

#[tokio::test]
async fn b_order_total_applies_the_configured_tax_rate() {
    ci::run(
        "checkout",
        "order total applies the configured tax rate",
        async {
            let total = order_total(100.0, &config_url()).await;
            assert_eq!(total, 125.0);
        },
    )
    .await;
}
