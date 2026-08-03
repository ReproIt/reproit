// Module under test for the flaky-CI fixture: computes an order total from
// the tax rate the config service answers.
//
// The planted bug: the config service's LEGACY format returns the rate as a
// STRING ("0.25"). `1 + rate` then concatenates before the multiply
// (1 + "0.25" === "10.25"), so a 100 subtotal totals 1025 instead of 125.
// FIXED=1 applies the fix: coerce the rate to a number before arithmetic.
export async function orderTotal(subtotal, configUrl) {
  const response = await fetch(configUrl + '/tax-rate');
  const { rate } = await response.json();
  const applied = process.env.FIXED === '1' ? Number(rate) : rate;
  return subtotal * (1 + applied);
}
