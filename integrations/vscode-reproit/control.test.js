"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const { readDescriptor, sendCommand, validateDescriptor } = require("./control");

function descriptor(port) {
  return {
    version: 1,
    sessionId: "run_1",
    occurrenceId: "occ_1",
    diagnosticReceiptId: "diag_1",
    state: "paused-before-trigger",
    controlEndpoint: { host: "127.0.0.1", port },
    authorizationToken: "a".repeat(48),
    debugger: "node-inspector",
    debuggerEndpoint: { host: "127.0.0.1", port: 9229 },
    sourceMappings: [],
    authoritative: false,
  };
}

test("descriptor validation rejects public control endpoints", () => {
  const value = descriptor(9000);
  value.controlEndpoint.host = "0.0.0.0";
  assert.throws(() => validateDescriptor(value), /strict validation/);
});

test("private descriptor files are bounded and parsed", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "reproit-vscode-"));
  const file = path.join(directory, "debug-session.json");
  fs.writeFileSync(file, JSON.stringify(descriptor(9000)), { mode: 0o600 });
  assert.equal(readDescriptor(file).occurrenceId, "occ_1");
  fs.rmSync(directory, { recursive: true });
});

test("control command round trips over loopback", async () => {
  const server = net.createServer((socket) => {
    let request = "";
    socket.on("data", (chunk) => (request += chunk));
    socket.on("end", () => {
      const parsed = JSON.parse(request);
      assert.equal(parsed.command, "replay-trigger");
      socket.end(JSON.stringify({
        version: 1,
        accepted: true,
        state: "triggering",
      }));
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  const response = await sendCommand(descriptor(address.port), "replay-trigger");
  assert.equal(response.accepted, true);
  await new Promise((resolve) => server.close(resolve));
});
