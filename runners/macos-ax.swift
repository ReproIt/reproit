// Compiles the responsibility-split macOS AX runner, then forwards its marker stream.
import Foundation

let runnerRoot = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
let sourceRoot = runnerRoot.appendingPathComponent("macos-ax")
let sources = [
  "signature.swift",
  "accessibility.swift",
  "runtime.swift",
  "main.swift",
].map { sourceRoot.appendingPathComponent($0).path }
let scratch = FileManager.default.temporaryDirectory
  .appendingPathComponent("reproit-macos-ax-" + UUID().uuidString)
try FileManager.default.createDirectory(
  at: scratch,
  withIntermediateDirectories: true
)
defer { try? FileManager.default.removeItem(at: scratch) }

let executable = scratch.appendingPathComponent("reproit-macos-ax").path
let compiler = Process()
compiler.executableURL = URL(fileURLWithPath: "/usr/bin/xcrun")
compiler.arguments = ["swiftc"] + sources + ["-o", executable]
compiler.standardOutput = FileHandle.standardError
compiler.standardError = FileHandle.standardError
try compiler.run()
compiler.waitUntilExit()
guard compiler.terminationStatus == 0 else {
  exit(compiler.terminationStatus)
}

let runner = Process()
runner.executableURL = URL(fileURLWithPath: executable)
runner.arguments = Array(CommandLine.arguments.dropFirst())
runner.environment = ProcessInfo.processInfo.environment
runner.standardInput = FileHandle.standardInput
runner.standardOutput = FileHandle.standardOutput
runner.standardError = FileHandle.standardError
try runner.run()
runner.waitUntilExit()
exit(runner.terminationStatus)
