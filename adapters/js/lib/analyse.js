'use strict';
// Analyse a list of files and return contract-v1 records (see docs/adapters/CONTRACT.md, section 5).
// No git and no process output here: the caller gives file paths and a way to read them.

const { parseFile, positionOf, openParenIndex } = require('./parse-file.js');
const { Resolver } = require('./resolve.js');

const CONTRACT = 1;
const LANGUAGE = 'javascript';
const TOOL = 'aft-js-adapter';
const TOOL_VERSION = '1.0.0';

// Minified files and bundles have no useful structure and are slow to parse. They are skipped
// without a record, as if they were not source code.
function isBundle(rel, src) {
  if (/\.min\.c?js$/.test(rel)) return true;
  const lines = src.split('\n').length;
  return src.length > 20000 && src.length / lines > 500;
}

function isSourcePath(rel) {
  return /\.c?js$/.test(rel) && !rel.split('/').includes('node_modules');
}

// rels: root-relative paths with "/" separators. readSource(rel) returns the file text (it may throw).
// Returns { records: [...], skippedBundles: [...] }.
function analyse(rels, readSource) {
  const files = new Map();
  const unanalysed = [];
  const skippedBundles = [];

  for (const rel of rels.filter(isSourcePath).sort()) {
    try {
      const src = readSource(rel);
      if (isBundle(rel, src)) {
        skippedBundles.push(rel);
        continue;
      }
      files.set(rel, parseFile(rel, src));
    } catch (error) {
      unanalysed.push({ type: 'unanalysed', files: [rel], reason: `could not parse: ${error.message}` });
    }
  }

  const resolver = new Resolver(files);
  const sites = [];
  let targetCount = 0;
  for (const file of files.values()) {
    const fileSites = [];
    for (const callSite of file.callSites) {
      const resolved = resolver.resolveCall(callSite, file);
      if (resolved === null) continue;
      const paren = openParenIndex(file.src, callSite.node.callee.end);
      if (paren === -1) continue;
      const at = positionOf(file, paren);
      const record = { type: 'site', file: file.rel, line: at.line, col: at.col, name: resolved.name };
      record.targets = resolved.targets.map((target) => {
        const position = positionOf(target.file, target.at);
        return { file: target.file.rel, line: position.line, col: position.col, name: target.name, kind: target.kind };
      });
      if (resolved.external.length > 0) record.external = resolved.external;
      fileSites.push(record);
      targetCount += record.targets.length;
    }
    fileSites.sort((a, b) => a.line - b.line || a.col - b.col);
    sites.push(...fileSites);
  }

  const analysedFiles = [...files.keys()];
  const records = [
    { type: 'header', contract: CONTRACT, language: LANGUAGE, tool: TOOL, tool_version: TOOL_VERSION },
    { type: 'analysed', files: analysedFiles },
    ...sites,
    ...unanalysed,
    {
      type: 'end',
      sites: sites.length,
      targets: targetCount,
      facts: 0,
      analysed_files: analysedFiles.length,
      unanalysed_files: unanalysed.length,
    },
  ];
  return { records, skippedBundles };
}

module.exports = { analyse, isSourcePath, isBundle };
