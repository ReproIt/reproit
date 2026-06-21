// A tiny counter app with one REAL, deterministic UI bug.
//
// BUG: the "Reset" button calls state.reset(), but `state` is a plain object
// with no `reset` method. Every click on Reset throws an uncaught TypeError
// ("state.reset is not a function"), which reproit's crash oracle catches via
// the page's `pageerror` event. The bug is deterministic: it fires on every
// Reset click, regardless of count value.
//
// The fix is to define reset() on the state object (see README).

const state = {
  count: 0,
  increment() { this.count += 1; },
  decrement() { this.count -= 1; },
  // NOTE: no reset() method defined -> Reset button crashes.
};

function render() {
  document.getElementById('value').textContent = String(state.count);
}

document.getElementById('inc').addEventListener('click', () => {
  state.increment();
  render();
});

document.getElementById('dec').addEventListener('click', () => {
  state.decrement();
  render();
});

document.getElementById('reset').addEventListener('click', () => {
  // Deterministic crash: state.reset is undefined.
  state.reset();
  render();
});

render();
