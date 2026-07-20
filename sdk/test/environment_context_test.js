'use strict';

const assert = require('assert');
const path = require('path');

Object.defineProperty(globalThis, 'navigator', {
  configurable: true,
  value: {
    language: 'en-US',
    userAgent:
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ' +
      'AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36',
  },
});
globalThis.window = {
  devicePixelRatio: 2,
  innerHeight: 900,
  innerWidth: 1440,
};
globalThis.document = {
  documentElement: {
    clientHeight: 900,
    clientWidth: 1440,
  },
};

const sdkPath = process.env.REPROIT_WEB_SDK
  ? path.resolve(process.env.REPROIT_WEB_SDK)
  : path.resolve(__dirname, '..', 'reproit-web.js');
const ReproIt = require(sdkPath);

const environment = ReproIt.environmentContext();
assert.deepStrictEqual(environment, {
  platform: 'web',
  browser: 'Chrome',
  browserMajor: '136',
  os: 'macOS',
  device: 'desktop',
  locale: 'en-US',
  viewport: {
    width: 1440,
    height: 900,
    dpr: 2,
  },
});
assert.equal(JSON.stringify(environment).includes(navigator.userAgent), false);

let request;
globalThis.fetch = (url, options) => {
  request = { url, options };
  return { catch() {} };
};
ReproIt._cfg = {
  appId: 'environment-test',
  context: { plan: 'team' },
  endpoint: 'https://ingest.example/v1/events',
  key: 'pk_test',
};
ReproIt._build = { version: '1.2.3' };
ReproIt._buf = [{ kind: 'error', message: 'test' }];
ReproIt._flush();

const body = JSON.parse(request.options.body);
const findingContext = body.frames[0].event.context;
assert.equal(findingContext.browser, 'Chrome');
assert.equal(findingContext.os, 'macOS');
assert.equal(findingContext.plan, 'team');
assert.equal(findingContext.build.version, '1.2.3');
assert.equal(JSON.stringify(body).includes(navigator.userAgent), false);

console.log('environment context: ok');
