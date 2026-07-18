import { IndicatorRelations } from '../src/indicator-relation';

test('indicator proof needs two stable samples and abstains during animation', () => {
  const relations = new IndicatorRelations();
  let animating = false;
  relations.register('liked', {
    dependentKey: 'key:badge',
    ownerKey: 'key:liked',
    containerKey: 'key:tabs',
    sample: () => ({
      indicator: { x: 180, y: 800, width: 10, height: 10 },
      owner: { x: 160, y: 700, width: 60, height: 50 },
      container: { x: 0, y: 680, width: 390, height: 100 },
      animating,
    }),
  });
  expect(relations.marker()).toBeNull();
  expect(relations.marker()).toContain('escaped-container');
  animating = true;
  expect(relations.marker()).toBeNull();
  expect(relations.marker()).toContain('ABSTAIN');
});
