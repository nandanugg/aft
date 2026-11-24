#!/usr/bin/env node
'use strict';
// aft language adapter for JavaScript (CommonJS), contract version 1.
//
//   node aft-js-adapter.js --root <repository>
//
// Reads the repository's tracked .js and .cjs files, resolves require() calls between them, and
// prints contract-v1 records on stdout, one JSON object per line. It never writes into the repository.
// Exit code 0 means the run finished. Files that do not parse are reported as "unanalysed".

const childProcess = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { analyse } = require('./lib/analyse.js');

function parseArguments(argv) {
  const index = argv.indexOf('--root');
  if (index === -1 || index + 1 >= argv.length) {
    throw new Error('usage: aft-js-adapter.js --root <repository>');
  }
  return path.resolve(argv[index + 1]);
}

function trackedFiles(root) {
  const output = childProcess.execFileSync('git', ['ls-files', '-z'], {
    cwd: root,
    encoding: 'utf8',
    maxBuffer: 512 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  return output.split('\0').filter((name) => name !== '');
}

function main() {
  let root;
  let rels;
  try {
    root = parseArguments(process.argv.slice(2));
    rels = trackedFiles(root);
  } catch (error) {
    process.stderr.write(`aft-js-adapter: ${error.message}\n`);
    process.exitCode = 2;
    return;
  }

  const { records, skippedBundles } = analyse(rels, (rel) => fs.readFileSync(path.join(root, rel), 'utf8'));
  if (skippedBundles.length > 0) {
    process.stderr.write(`aft-js-adapter: skipped ${skippedBundles.length} minified or bundled file(s)\n`);
  }
  process.stdout.write(records.map((record) => `${JSON.stringify(record)}\n`).join(''));
}

main();
