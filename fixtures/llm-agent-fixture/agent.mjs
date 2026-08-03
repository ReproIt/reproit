// Planted agent-bug fixture: an Express app whose /assist operation runs a
// small tool-calling agent against a loopback fake LLM API. The model
// STREAMS its reply (SSE) and directs a tool call; the planted bug is a
// specific model response that names the WRONG tool (delete_order instead of
// refund_order). The app executes it, its guardrail ledger catches the
// destructive call after the fact, marks the agent-guardrail-violation
// oracle on the trace, and answers 500.
//
// MODE=capture: boots the fake LLM API + fake tool service + the app, fires
// the failing request, writes a version-2 reproit-backend-capture
// (exchanges + envelope, oracle from the trace marker) to CAPTURE_OUT.
// Default (server) mode: boots ONLY the app on $PORT; with REPROIT_REPLAY
// set the SDK serves the recorded model/tool exchanges, so no model API and
// no tool service exist. FIXED=1 applies the fix: the tool allowlist is
// enforced BEFORE execution, the destructive call is refused, and the app
// answers 200 with a safe refusal.
import { createRequire } from 'node:module';
import http from 'node:http';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const SDK = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../sdk/reproit-backend-node',
);
const require = createRequire(SDK + '/index.js');
const sdk = require(SDK + '/index.js');
const reproitExpress = require(SDK + '/express.js');
const instrument = require(SDK + '/instrument.js');
const express = require(SDK + '/node_modules/express');

instrument.install();

const LLM_PORT = 19981;
const TOOL_PORT = 19982;
const APP_PORT = 19980;
// The agent's authored loop bound; exceeding it marks agent-loop-bound-exceeded.
const LOOP_BOUND = 4;
// Tools the operator allows an agent to run for /assist.
const ALLOWED_TOOLS = ['lookup_order', 'refund_order'];

// The model's scripted turns for "refund order 42". Turn one streams a reply
// that directs the WRONG, destructive tool (the planted bug); turn two
// acknowledges the tool result and finishes.
const MODEL_TURNS = [
  {
    text: 'Handling the refund for order 42.',
    tool: { name: 'delete_order', input: { order: 42 } },
  },
  { text: 'Order 42 has been processed.', tool: null },
];

// SSE frames for one model turn, Anthropic-shaped: text deltas first, then
// the tool directive, then the stop event, each frame its own chunk.
function sseFrames(turn) {
  const frames = turn.text
    .split(' ')
    .map((word) => ({ type: 'content_block_delta', delta: { text: word + ' ' } }));
  if (turn.tool) frames.push({ type: 'tool_use', name: turn.tool.name, input: turn.tool.input });
  frames.push({ type: 'message_stop' });
  return frames.map((frame) => 'data: ' + JSON.stringify(frame) + '\n\n');
}

// Parse one streamed model turn back out of the SSE text.
function parseTurn(sseText) {
  const turn = { text: '', tool: null };
  for (const line of sseText.split('\n')) {
    if (!line.startsWith('data: ')) continue;
    const frame = JSON.parse(line.slice('data: '.length));
    if (frame.type === 'content_block_delta') turn.text += frame.delta.text;
    if (frame.type === 'tool_use') turn.tool = { name: frame.name, input: frame.input };
  }
  turn.text = turn.text.trim();
  return turn;
}

async function callModel(messages) {
  const response = await fetch('http://127.0.0.1:' + LLM_PORT + '/v1/messages', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'fake-model-1', stream: true, messages }),
  });
  if (response.status !== 200) throw new Error('model API answered ' + response.status);
  // Consume the stream incrementally, the way a real agent SDK does; under
  // replay the recorded chunk boundaries re-serve this exact shape.
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let sseText = '';
  let chunks = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    sseText += decoder.decode(value, { stream: true });
    chunks += 1;
  }
  return { turn: parseTurn(sseText), chunks };
}

async function runTool(tool) {
  const response = await fetch('http://127.0.0.1:' + TOOL_PORT + '/tools/' + tool.name, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(tool.input),
  });
  return response.json();
}

function buildApp(capture) {
  const app = express();
  app.use(express.json());
  app.use(reproitExpress({ capture }));
  app.post('/assist', async (req, res) => {
    const fixed = process.env.FIXED === '1';
    const trace = req.reproit ?? null;
    const executed = [];
    try {
      const messages = [{ role: 'user', content: String(req.body?.question ?? '') }];
      let iterations = 0;
      let answer = null;
      for (;;) {
        if (iterations >= LOOP_BOUND) {
          if (trace) trace.oracle(sdk.AGENT_LOOP_BOUND_ORACLE, { iterations, bound: LOOP_BOUND });
          return res.status(500).json({ error: 'agent exceeded its loop bound' });
        }
        iterations += 1;
        const { turn } = await callModel(messages);
        messages.push({ role: 'assistant', content: turn.text });
        if (!turn.tool) {
          answer = turn.text;
          break;
        }
        // THE FIX: enforce the tool allowlist BEFORE execution. The planted
        // bug ships without it, executes the model's destructive directive,
        // and only the after-the-fact ledger audit below catches it.
        if (fixed && !ALLOWED_TOOLS.includes(turn.tool.name)) {
          return res.json({
            answer: 'I cannot run ' + turn.tool.name + '; it is not an allowed tool.',
            refusedTool: turn.tool.name,
            iterations,
          });
        }
        const result = await runTool(turn.tool);
        executed.push(turn.tool.name);
        // Format the tool result field by field, deterministically. A raw
        // JSON.stringify of the parsed result would embed the serializer's
        // key order into the next prompt, which is exactly the
        // request-nondeterminism the capsule's strict matching rejects.
        messages.push({
          role: 'user',
          content: 'tool ' + turn.tool.name + ': ' + String(result.status),
        });
      }
      // The buggy build only audits the ledger AFTER the loop finished, so
      // the destructive call has already run by the time it is noticed.
      const violation = executed.find((name) => !ALLOWED_TOOLS.includes(name));
      if (violation) {
        if (trace) trace.oracle(sdk.AGENT_GUARDRAIL_ORACLE, { tool: violation });
        return res.status(500).json({ error: 'guardrail violated: ' + violation });
      }
      res.json({ answer, iterations });
    } catch (err) {
      res.status(500).json({ error: 'internal' });
    }
  });
  return app;
}

function startFakeLlm() {
  let turn = 0;
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      const frames = sseFrames(MODEL_TURNS[Math.min(turn, MODEL_TURNS.length - 1)]);
      turn += 1;
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      let index = 0;
      const send = () => {
        if (index >= frames.length) return res.end();
        res.write(frames[index++]);
        setTimeout(send, 2);
      };
      send();
    });
  });
  return new Promise((resolve) => server.listen(LLM_PORT, () => resolve(server)));
}

function startFakeTools() {
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ tool: req.url.split('/').pop(), status: 'done' }));
    });
  });
  return new Promise((resolve) => server.listen(TOOL_PORT, () => resolve(server)));
}

if (process.env.MODE === 'capture') {
  const llm = await startFakeLlm();
  const tools = await startFakeTools();
  const fileCapture = {
    context() {
      return {
        traceId: 'cap-agent-1',
        actor: null,
        actionIndex: 0,
        build: 'agent-fixture',
        configContract: null,
        captureEnvelope: true,
      };
    },
    record(trace) {
      const payload = {
        format: 'reproit-backend-capture',
        version: 2,
        operation: trace.events()[0].operation,
        oracle: sdk.markedOracle(trace.events()) ?? sdk.SERVER_ERROR_ORACLE,
        envelope: {
          observedAtMs: Date.now(),
          tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
          node: process.version,
          os: process.platform,
          arch: process.arch,
          replaySeed: 'ab42ab42ab42ab42',
        },
        events: trace.events(),
      };
      fs.writeFileSync(process.env.CAPTURE_OUT, sdk.canonicalJson(payload));
    },
  };
  const app = buildApp(fileCapture);
  const server = app.listen(APP_PORT, async () => {
    const res = await fetch('http://127.0.0.1:' + APP_PORT + '/assist', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ question: 'refund order 42' }),
    });
    console.log('capture fixture status', res.status);
    server.close();
    llm.close();
    tools.close();
  });
} else {
  const app = buildApp(null);
  const port = Number(process.env.PORT ?? APP_PORT);
  app.listen(port, () => console.log('serving on', port));
}
