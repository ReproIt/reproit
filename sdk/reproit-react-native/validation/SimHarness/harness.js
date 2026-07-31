// Device harness for the React Native SDK's capture path.
//
// Runs the REAL built dist modules (exchange, capture-batch, causal) inside a
// WKWebView on the simulator, wrapping WebKit's genuine fetch. No fetch shim
// is written here: installCausalFetch wraps whatever fetch the host provides,
// exactly as it does on a device under React Native.
async function main() {
  const params = new URLSearchParams(location.search);
  const phase = params.get('phase') || 'capture';
  const dependency = params.get('dependency');
  const ingest = params.get('ingest');
  const unmatched = params.get('unmatched');
  const out = (text) => {
    const line = document.createElement('div');
    line.textContent = 'RP: ' + text;
    document.body.appendChild(line);
    window.webkit.messageHandlers.rp.postMessage('RP: ' + text);
  };

  const causal = require('./causal');
  const batch = require('./capture-batch');
  out('phase=' + phase);

  const recorded = [];
  let capsule;
  if (phase !== 'capture') {
    capsule = JSON.parse(decodeURIComponent(params.get('capsule') || '{}'));
    // The documented embedded-host override the SDK reads before touching
    // react-native's NativeModules.
    globalThis.__reproit_capsule = capsule;
  }

  causal.installCausalFetch({
    actionIndex: () => 0,
    capsule: phase === 'capture' ? undefined : causal.nativeCausalCapsule(),
    excludePrefix: ingest,
    emitMarker: phase !== 'capture',
    record: phase === 'capture' ? (x) => recorded.push(x) : undefined,
  });

  const target = phase === 'miss' ? unmatched : dependency;
  let payload;
  try {
    const response = await fetch(target);
    out('http-status=' + response.status);
    const text = await response.text();
    out('http-body=' + text);
    payload = JSON.parse(text);
  } catch (error) {
    out('network-error=' + String(error && error.message ? error.message : error));
    out('result=NETWORK-FAILED');
    return;
  }

  if (Array.isArray(payload && payload.prices)) {
    out('result=OK-UNEXPECTED');
    return;
  }
  out('planted-failure=TypeError: prices is not an array');

  if (phase === 'capture') {
    out('recorded-exchanges=' + recorded.length);
    const envelope = batch.buildEnvelope({
      observedAtMs: Date.now(),
      platform: 'ios',
      osVersion: String(navigator.userAgent || 'webview'),
      locale: navigator.language || 'en',
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
      replaySeed: batch.replaySeed(),
    });
    const built = batch.buildCaptureBatch({
      appId: 'sim-rn',
      sessionId: 'sess-rn-1',
      batchId: 'cb-rn-webview-1',
      deployment: { version: '1.4.2', commit: 'abc123def456' },
      occurrence: {
        operation: 'quote-screen',
        trigger: { subject: 'tap:quote' },
        exchanges: recorded,
        failure: {
          oracle: 'crash',
          summary: 'TypeError: prices is not an array',
          signature: 'crash:quote-screen',
          observationPoint: 'quote-screen',
        },
        envelope: envelope,
      },
    });
    if (!built) {
      out('result=NO-BATCH');
      return;
    }
    await fetch(ingest + '/v1/capture-batches', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(built),
    });
    out('result=FAILED-AS-PLANTED');
    return;
  }
  out('result=FAILED-AS-PLANTED');
}

main().catch((error) => {
  window.webkit.messageHandlers.rp.postMessage('RP: harness-error=' + String(error));
});
