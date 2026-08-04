"use strict";

const fs = require("node:fs");
const net = require("node:net");

const MAX_DESCRIPTOR_BYTES = 64 * 1024;
const MAX_RESPONSE_BYTES = 8 * 1024;

function readDescriptor(path) {
  const stat = fs.statSync(path);
  if (!stat.isFile() || stat.size === 0 || stat.size > MAX_DESCRIPTOR_BYTES) {
    throw new Error("Reproit session descriptor is empty or exceeds 64 KiB");
  }
  const descriptor = JSON.parse(fs.readFileSync(path, "utf8"));
  validateDescriptor(descriptor);
  return descriptor;
}

function validateDescriptor(descriptor) {
  if (
    descriptor.version !== 1 ||
    descriptor.authoritative !== false ||
    descriptor.controlEndpoint?.host !== "127.0.0.1" ||
    !Number.isInteger(descriptor.controlEndpoint?.port) ||
    descriptor.controlEndpoint.port < 1 ||
    descriptor.controlEndpoint.port > 65535 ||
    typeof descriptor.authorizationToken !== "string" ||
    !/^[0-9a-fA-F]{32,128}$/.test(descriptor.authorizationToken) ||
    typeof descriptor.occurrenceId !== "string" ||
    !/^occ_[A-Za-z0-9_-]+$/.test(descriptor.occurrenceId)
  ) {
    throw new Error("Reproit session descriptor failed strict validation");
  }
}

function sendCommand(descriptor, command, timeoutMs = 5000) {
  validateDescriptor(descriptor);
  if (![
    "status",
    "debugger-attached",
    "replay-trigger",
    "stop",
  ].includes(command)) {
    return Promise.reject(new Error(`Unsupported Reproit command: ${command}`));
  }
  const request = JSON.stringify({
    version: 1,
    authorizationToken: descriptor.authorizationToken,
    command,
  });
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({
      host: descriptor.controlEndpoint.host,
      port: descriptor.controlEndpoint.port,
    });
    let settled = false;
    let bytes = Buffer.alloc(0);
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      if (error) reject(error);
      else resolve(value);
    };
    socket.setTimeout(timeoutMs, () => finish(new Error("Reproit control request timed out")));
    socket.on("connect", () => socket.end(request));
    socket.on("data", (chunk) => {
      bytes = Buffer.concat([bytes, chunk]);
      if (bytes.length > MAX_RESPONSE_BYTES) {
        finish(new Error("Reproit control response exceeded 8 KiB"));
      }
    });
    socket.on("error", (error) => finish(error));
    socket.on("close", () => {
      if (settled) return;
      try {
        const response = JSON.parse(bytes.toString("utf8"));
        if (response.version !== 1 || typeof response.accepted !== "boolean") {
          throw new Error("Reproit control response failed validation");
        }
        finish(null, response);
      } catch (error) {
        finish(error);
      }
    });
  });
}

module.exports = { readDescriptor, sendCommand, validateDescriptor };
