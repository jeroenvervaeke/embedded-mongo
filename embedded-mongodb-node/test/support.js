'use strict';

const fs = require('node:fs');
const path = require('node:path');

/**
 * A fresh directory under `target`, never the system one: /tmp is a memory filesystem on a
 * good many Linux machines, and the engine allocates a journal for every directory it opens.
 */
function scratch() {
  const base = path.join(__dirname, '..', '..', 'target', 'tmp');
  fs.mkdirSync(base, { recursive: true });
  return fs.mkdtempSync(path.join(base, 'node-'));
}

module.exports = { scratch };
