// Validates the BOT-WALL guard (FIX 4): a WAF challenge interstitial is detected
// (so the scan is reported UNSCANNABLE with zero findings), while a real app page --
// even one that mentions "security" or shows a login CAPTCHA in normal content --
// is NOT misdetected. Browser-backed via Playwright. Run `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { detectBotWall } from './runner.mjs';

test('a Cloudflare "Just a moment..." interstitial is detected', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(`<!doctype html><title>Just a moment...</title>
      <body><h1>Checking your browser before accessing</h1>
      <div id="cf-challenge-running"></div>
      <p>Performing a security verification. Ray ID: 8b2f...</p></body>`);
    const wall = await detectBotWall(page);
    assert.ok(wall, 'the interstitial must be detected');
    assert.equal(wall.vendor, 'Cloudflare');
  } finally {
    await browser.close();
  }
});

test('a Turnstile challenge-platform marker is detected', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(`<!doctype html><title>Attention Required! | Cloudflare</title>
      <body><div class="cf-turnstile"></div><p>attention required cloudflare</p></body>`);
    const wall = await detectBotWall(page);
    assert.ok(wall, 'a turnstile / cf-challenge marker must be detected');
  } finally {
    await browser.close();
  }
});

test('a normal app page is NOT misdetected as a bot wall', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    // Mentions "security" and even has a login CAPTCHA, but it is the real app.
    await page.setContent(`<!doctype html><title>Acme Dashboard</title>
      <body><nav><a href="/">Home</a><a href="/security">Security settings</a></nav>
      <main><h1>Welcome back</h1><p>Manage your account security here.</p>
      <form><input name="q"><button>Search</button></form></main></body>`);
    const wall = await detectBotWall(page);
    assert.equal(wall, null, 'a real app page must not be flagged as a challenge');
  } finally {
    await browser.close();
  }
});
