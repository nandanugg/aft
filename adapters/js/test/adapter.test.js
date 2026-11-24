'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const childProcess = require('node:child_process');
const { analyse } = require('../lib/analyse.js');
const { packageName } = require('../lib/resolve.js');

const FIXTURE = path.join(__dirname, 'fixtures', 'cjs');
const ADAPTER = path.join(__dirname, '..', 'aft-js-adapter.js');

function listFiles(dir, prefix = '') {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const rel = prefix + entry.name;
    return entry.isDirectory() ? listFiles(path.join(dir, entry.name), `${rel}/`) : [rel];
  });
}

function run(root, files) {
  const { records } = analyse(files, (rel) => fs.readFileSync(path.join(root, rel), 'utf8'));
  return records;
}

const records = run(FIXTURE, listFiles(FIXTURE));
const sites = records.filter((r) => r.type === 'site');

// All calls on one line of the main spec are listed by name, so each assertion names its line text.
function lineOf(file, text) {
  const lines = fs.readFileSync(path.join(FIXTURE, file), 'utf8').split('\n');
  const index = lines.findIndex((line) => line.includes(text));
  assert.notStrictEqual(index, -1, `text not found: ${text}`);
  return index + 1;
}

function siteAt(file, text) {
  const line = lineOf(file, text);
  return sites.find((s) => s.file === file && s.line === line);
}

function targetOf(site) {
  assert.ok(site, 'site expected');
  assert.strictEqual(site.targets.length, 1, 'one target expected');
  const t = site.targets[0];
  return `${t.file}:${t.line}:${t.col}`;
}

const MAIN = 'specs/main.spec.js';

test('header, analysed list and end counts are consistent', () => {
  assert.deepStrictEqual(records[0], { type: 'header', contract: 1, language: 'javascript', tool: 'aft-js-adapter', tool_version: '1.0.0' });
  const end = records[records.length - 1];
  assert.strictEqual(end.type, 'end');
  assert.strictEqual(end.sites, sites.length);
  assert.strictEqual(end.targets, sites.reduce((n, s) => n + s.targets.length, 0));
  assert.strictEqual(end.facts, 0);
  const analysed = records.find((r) => r.type === 'analysed').files;
  assert.strictEqual(end.analysed_files, analysed.length);
  assert.ok(!analysed.includes('specs/broken.spec.js'));
});

test('a file that does not parse is unanalysed, the rest is still analysed', () => {
  const unanalysed = records.filter((r) => r.type === 'unanalysed');
  assert.strictEqual(unanalysed.length, 1);
  assert.deepStrictEqual(unanalysed[0].files, ['specs/broken.spec.js']);
  assert.match(unanalysed[0].reason, /could not parse/);
  assert.strictEqual(records[records.length - 1].unanalysed_files, 1);
});

test('destructured require, with and without rename', () => {
  assert.strictEqual(targetOf(siteAt(MAIN, 'await findItems(1)')), 'utils/helpers.js:1:7');
  assert.strictEqual(targetOf(siteAt(MAIN, 'wait(2)')), 'utils/helpers.js:3:10');
});

test('whole-module require followed by a member call', () => {
  assert.strictEqual(targetOf(siteAt(MAIN, 'helpers.waitFor(3)')), 'utils/helpers.js:3:10');
});

test('require(...).f() call', () => {
  assert.strictEqual(targetOf(siteAt(MAIN, "require('../utils/helpers').findItems(4)")), 'utils/helpers.js:1:7');
});

test('the site position is the open parenthesis', () => {
  const site = siteAt(MAIN, 'helpers.waitFor(3)');
  const text = fs.readFileSync(path.join(FIXTURE, MAIN), 'utf8').split('\n')[site.line - 1];
  assert.strictEqual(text[site.col - 1], '(');
  assert.strictEqual(site.name, 'waitFor');
});

test('same module, different reset: only the right module is a target', () => {
  assert.strictEqual(targetOf(siteAt(MAIN, 'recorder.reset()')), 'utils/recorder.js:7:3');
  assert.strictEqual(targetOf(siteAt(MAIN, 'other.reset()')), 'utils/other-recorder.js:1:10');
});

test('a file-local function with the same name is not the other module function', () => {
  assert.strictEqual(targetOf(siteAt(MAIN, '  reset();')), `${MAIN}:12:10`);
});

test('module.exports = new Class(): calls reach the class method of that module only', () => {
  assert.strictEqual(targetOf(siteAt(MAIN, 'APage.submit()')), 'pages/a.page.js:2:9');
  assert.strictEqual(targetOf(siteAt(MAIN, 'BPage.submit()')), 'pages/b.page.js:2:9');
});

test('re-export chains: require of an index file, spread, and exports.x', () => {
  assert.strictEqual(targetOf(siteAt(MAIN, 'lib.hello()')), 'lib/deep/inner.js:1:16');
  assert.strictEqual(targetOf(siteAt(MAIN, 'barrel.record()')), 'utils/recorder.js:4:3');
  assert.strictEqual(targetOf(siteAt(MAIN, 'barrel.findItems(5)')), 'utils/helpers.js:1:7');
});

test('exports.x assigned after module.exports = ... is ignored (the old object is not exported)', () => {
  const unresolved = sites.filter((s) => s.file === 'lib/deep/inner.js');
  assert.deepStrictEqual(unresolved, []);
});

test('a package or built-in call is an external label with no target', () => {
  const site = siteAt(MAIN, "path.join('a', 'b')");
  assert.deepStrictEqual(site.targets, []);
  assert.deepStrictEqual(site.external, [{ name: 'join', package: 'path' }]);
});

test('json requires and unknown globals get no record', () => {
  assert.strictEqual(siteAt(MAIN, 'data.thing()'), undefined);
  assert.strictEqual(siteAt(MAIN, 'unknownGlobal()'), undefined);
});

test('a parameter that shadows an import is not resolved', () => {
  assert.strictEqual(siteAt('specs/shadow.spec.js', 'findItems(1);'), undefined);
});

test('a variable that is assigned again is not trusted', () => {
  assert.strictEqual(siteAt('specs/shadow.spec.js', 'later.findItems(2)'), undefined);
});

test('package names', () => {
  assert.strictEqual(packageName('node:fs'), 'fs');
  assert.strictEqual(packageName('lodash/fp'), 'lodash');
  assert.strictEqual(packageName('@scope/pkg/sub'), '@scope/pkg');
});

test('columns count UTF-8 bytes', () => {
  const files = { 'a.js': "const f = () => {};\nconst s = 'é€'; f();\n" };
  const { records: out } = analyse(Object.keys(files), (rel) => files[rel]);
  const site = out.find((r) => r.type === 'site');
  assert.strictEqual(site.line, 2);
  // `const s = 'é€'; f` is 20 bytes before the `(`: é is 2 bytes and € is 3 bytes.
  assert.strictEqual(site.col, 'const s = \'é€\'; f'.length + 3 + 1);
});

test('node_modules and minified files are skipped', () => {
  const files = { 'node_modules/x/index.js': 'f();', 'dist/app.min.js': 'f();', 'ok.js': 'function f(){}\nf();\n' };
  const { records: out, skippedBundles } = analyse(Object.keys(files), (rel) => files[rel]);
  assert.deepStrictEqual(out.find((r) => r.type === 'analysed').files, ['ok.js']);
  assert.deepStrictEqual(skippedBundles, ['dist/app.min.js']);
});

test('command line: prints valid records for a git repository and does not change it', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aft-js-adapter-'));
  try {
    fs.cpSync(FIXTURE, root, { recursive: true });
    childProcess.execFileSync('git', ['init', '-q'], { cwd: root });
    childProcess.execFileSync('git', ['add', '-A'], { cwd: root });
    const before = childProcess.execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' });
    const result = childProcess.spawnSync('node', [ADAPTER, '--root', root], { encoding: 'utf8' });
    assert.strictEqual(result.status, 0, result.stderr);
    assert.ok(result.stdout.endsWith('\n'));
    const lines = result.stdout.trimEnd().split('\n').map((l) => JSON.parse(l));
    assert.strictEqual(lines[0].type, 'header');
    assert.strictEqual(lines[lines.length - 1].type, 'end');
    const after = childProcess.execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' });
    assert.strictEqual(after, before);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('command line: a missing --root is a usage error', () => {
  const result = childProcess.spawnSync('node', [ADAPTER], { encoding: 'utf8' });
  assert.strictEqual(result.status, 2);
  assert.match(result.stderr, /usage/);
});
