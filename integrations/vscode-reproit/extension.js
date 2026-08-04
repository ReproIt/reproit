"use strict";

const fs = require("node:fs");
const path = require("node:path");
const vscode = require("vscode");
const { readDescriptor, sendCommand } = require("./control");

function activate(context) {
  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  status.command = "reproit.openEvidence";
  context.subscriptions.push(status);

  const withSession = async (operation) => {
    try {
      const descriptorPath = sessionPath();
      const descriptor = readDescriptor(descriptorPath);
      const result = await operation(descriptor, descriptorPath);
      updateStatus(status, descriptorPath);
      return result;
    } catch (error) {
      vscode.window.showErrorMessage(`Reproit: ${error.message}`);
      return undefined;
    }
  };

  register(context, "reproit.debuggerAttached", () =>
    withSession(async (descriptor) => {
      await accepted(descriptor, "debugger-attached");
      vscode.window.showInformationMessage("Reproit debugger attachment recorded");
    }),
  );
  register(context, "reproit.replayTrigger", () =>
    withSession(async (descriptor) => {
      await accepted(descriptor, "replay-trigger");
      vscode.window.showInformationMessage("Reproit captured trigger released");
    }),
  );
  register(context, "reproit.stopSession", () =>
    withSession(async (descriptor) => {
      await accepted(descriptor, "stop");
      vscode.window.showInformationMessage("Reproit session is cleaning up");
    }),
  );
  register(context, "reproit.verifyAuthoritatively", () =>
    withSession(async (descriptor) => {
      const terminal = vscode.window.createTerminal("Reproit authoritative verification");
      terminal.show();
      terminal.sendText(`reproit ${shellQuote(descriptor.occurrenceId)}`, true);
    }),
  );
  register(context, "reproit.openEvidence", () =>
    withSession(async (_descriptor, descriptorPath) => {
      const document = await vscode.workspace.openTextDocument(descriptorPath);
      await vscode.window.showTextDocument(document, { preview: false });
    }),
  );

  updateStatus(status, safeSessionPath());
  const watcher = vscode.workspace.onDidChangeConfiguration((event) => {
    if (event.affectsConfiguration("reproit.debugSession")) {
      updateStatus(status, safeSessionPath());
    }
  });
  const debugWatcher = vscode.debug.onDidStartDebugSession(() => {
    void withSession(async (descriptor) => {
      await accepted(descriptor, "debugger-attached");
    });
  });
  context.subscriptions.push(watcher, debugWatcher);
}

function register(context, command, callback) {
  context.subscriptions.push(vscode.commands.registerCommand(command, callback));
}

async function accepted(descriptor, command) {
  const response = await sendCommand(descriptor, command);
  if (!response.accepted) {
    throw new Error(response.detail || `control command ${command} was rejected`);
  }
  return response;
}

function sessionPath() {
  const configured = vscode.workspace
    .getConfiguration("reproit")
    .get("debugSession", "")
    .trim();
  if (!configured || !path.isAbsolute(configured) || !fs.existsSync(configured)) {
    throw new Error("this workspace has no active private debug-session.json descriptor");
  }
  return configured;
}

function safeSessionPath() {
  try {
    return sessionPath();
  } catch (_error) {
    return undefined;
  }
}

function updateStatus(status, descriptorPath) {
  if (!descriptorPath) {
    status.hide();
    return;
  }
  try {
    const descriptor = readDescriptor(descriptorPath);
    status.text = `$(debug-alt) Reproit: ${descriptor.state}`;
    status.tooltip = "Open the private Reproit debug session evidence";
    status.show();
  } catch (_error) {
    status.hide();
  }
}

function shellQuote(value) {
  return `'${String(value).replaceAll("'", "'\\''")}'`;
}

function deactivate() {}

module.exports = { activate, deactivate };
