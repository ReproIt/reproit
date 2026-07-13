import test from 'node:test';
import assert from 'node:assert/strict';
import { responseShape } from './runner.mjs';

test('response shape is structural, sorted, and value independent', () => {
  const first = responseShape({ user: { name: 'Alice', age: 32 }, active: true });
  const second = responseShape({ active: false, user: { age: 99, name: 'Bob' } });
  assert.equal(first, second);
  assert.equal(first, '{active:boolean,user:{age:number,name:string}}');
});

test('array shape is order independent and bounded to element types', () => {
  assert.equal(responseShape([1, 'x', 2]), '[number|string]');
});
