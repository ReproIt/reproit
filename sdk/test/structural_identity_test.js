const assert = require('assert');
const ReproIt = require('../reproit-web.js');

assert.strictEqual(
  ReproIt.structuralMessage("Cannot read 'Ada' at line 42, column 7"),
  'Cannot read <q> at line # column #',
);
assert.strictEqual(
  ReproIt.structuralMessage("Cannot read 'Grace' at line 9001, column 12"),
  'Cannot read <q> at line # column #',
);

console.log('reproit-web structural identity: PASS');
