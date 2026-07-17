import { FocusVisibilityOracle } from '../src/focus-visibility';

test('proves only complete post-reveal obscuration after two stable samples', () => {
  const oracle = new FocusVisibilityOracle();
  let reveals = 0;
  oracle.register('email', {
    reveal: () => {
      reveals++;
      return true;
    },
    sample: () => ({
      key: 'key:email',
      focusedEditable: true,
      exactKeyboardRect: true,
      field: { x: 20, y: 760, width: 200, height: 40 },
      usableViewport: { x: 0, y: 0, width: 390, height: 500 },
    }),
  });
  expect(oracle.marker()).toBeNull();
  expect(reveals).toBe(1);
  expect(oracle.marker()).toBeNull();
  expect(oracle.marker()).toContain('focused-input-obscured:key:email');
});

test('abstains for floating keyboard and absent safe reveal', () => {
  for (const [exact, reveal] of [
    [false, true],
    [true, false],
  ] as const) {
    const oracle = new FocusVisibilityOracle();
    oracle.register('x', {
      reveal: () => reveal,
      sample: () => ({
        key: 'x',
        focusedEditable: true,
        exactKeyboardRect: exact,
        field: { x: 0, y: 600, width: 10, height: 10 },
        usableViewport: { x: 0, y: 0, width: 100, height: 100 },
      }),
    });
    expect(oracle.marker()).toBeNull();
    expect(oracle.marker()).toBeNull();
    expect(oracle.marker()).toBeNull();
  }
});
