import { test, expect } from '@playwright/test';

// The user's OWN test. It signs in (step 1) and stops. It never opens step 2,
// so it never sees the bugs that live there. reproit replays these actions to
// reach step 2, then fuzzes onward and finds what the test never covered.
const APP = process.env.DEMO_URL || 'http://localhost:8099/';

test('sign in reaches step 2', async ({ page }) => {
  await page.goto(APP);
  await page.getByTestId('username').fill('ada');
  await page.getByTestId('password').fill('secret123');
  await page.getByTestId('continue').click();
  // The test only asserts step 2 appeared. It never touches the buggy controls.
  await expect(page.getByRole('heading', { name: 'Step 2: account' })).toBeVisible();
});
