import Foundation

/// Drive the real `ReproItCausalURLProtocol` replay path into an unmatched
/// call on an iPhone simulator.
///
/// The capsule arrives through `REPROIT_CAPSULE_JSON` and the live call through
/// `PROBE_URL`, both derived from the shared vectors
/// (`sdk/capture-behavior-v1.json`, vocabularies.divergenceMarkers.parityScenario).
/// The SDK writes `REPROIT:DIVERGENCE` to stderr itself; this probe only
/// reports the frozen runner contract, which reaches the caller as a failed
/// request rather than a stream, on stdout.
///
/// Compiled as a single module with the SDK sources so the internal installer
/// is reachable without weakening its access level.
let target = URL(string: ProcessInfo.processInfo.environment["PROBE_URL"] ?? "")!
ReproItCausalURLProtocol.install(excluding: nil)
let configuration = URLSessionConfiguration.ephemeral
configuration.protocolClasses = [ReproItCausalURLProtocol.self]
let finished = DispatchSemaphore(value: 0)
URLSession(configuration: configuration).dataTask(with: target) { _, _, error in
  if let error {
    print("PROBE:MISS \((error as NSError).localizedDescription)")
  } else {
    print("PROBE:NOMISS")
  }
  finished.signal()
}.resume()
_ = finished.wait(timeout: .now() + 30)
