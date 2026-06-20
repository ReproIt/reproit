// swift-tools-version:5.9
import PackageDescription

// ReproIt iOS production telemetry SDK.
//
// The package is intentionally split so the canonical contract (state
// signature + payload encoding) is pure Foundation and therefore builds and
// tests on a macOS host with `swift test`. All UIKit capture code lives in the
// same target but is compiled only when UIKit is available (#if canImport),
// so the library itself is iOS-first while the parity test stays host-runnable.
let package = Package(
    name: "ReproIt",
    platforms: [
        .iOS(.v13),
        .macOS(.v11), // host build target for `swift test` parity coverage
    ],
    products: [
        .library(name: "ReproIt", targets: ["ReproIt"]),
    ],
    targets: [
        .target(name: "ReproIt"),
        .testTarget(name: "ReproItTests", dependencies: ["ReproIt"]),
    ]
)
