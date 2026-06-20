// swift-tools-version:5.9
import PackageDescription

// ReproIt production telemetry SDK (iOS UIKit + native macOS AppKit).
//
// The package is intentionally split so the canonical contract (state
// signature + payload encoding) is pure Foundation and therefore builds and
// tests on a macOS host with `swift test`. Platform capture lives in the same
// target but is compiled conditionally (#if canImport): UIKit capture
// (Capture.swift) on iOS / Catalyst, AppKit capture (CaptureAppKit.swift) on
// native macOS. Both walk the platform view tree into the SAME ReproItNode
// model and reuse Signature.swift unchanged, so every platform hashes
// byte-for-byte identically and the host parity test stays runnable.
let package = Package(
    name: "ReproIt",
    platforms: [
        .iOS(.v13),
        .macOS(.v11), // native macOS/AppKit production target + host parity test
    ],
    products: [
        .library(name: "ReproIt", targets: ["ReproIt"]),
    ],
    targets: [
        .target(name: "ReproIt"),
        .testTarget(name: "ReproItTests", dependencies: ["ReproIt"]),
    ]
)
