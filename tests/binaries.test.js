const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { test } = require('node:test');
const { ensureBundledBinaryExecutable } = require('../out/binaries');

const posix = process.platform !== 'win32';

function temporaryDirectory(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'quickql-permissions-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return directory;
}

test('bundled engine and language server can spawn after losing execute permissions', { skip: !posix }, t => {
  const directory = temporaryDirectory(t);
  for (const name of ['ql-engine', 'ql-lsp']) {
    const binary = path.join(directory, name);
    fs.writeFileSync(binary, '#!/bin/sh\nprintf "started"\n', { mode: 0o644 });
    assert.equal(spawnSync(binary).error.code, 'EACCES');

    ensureBundledBinaryExecutable(binary);

    const result = spawnSync(binary, [], { encoding: 'utf8' });
    assert.equal(result.error, undefined);
    assert.equal(result.status, 0);
    assert.equal(result.stdout, 'started');
    assert.equal(fs.statSync(binary).mode & 0o777, 0o744);
  }
});

test('already executable bundled binary keeps its permissions', { skip: !posix }, t => {
  const binary = path.join(temporaryDirectory(t), 'ql-engine');
  fs.writeFileSync(binary, '#!/bin/sh\n', { mode: 0o750 });
  ensureBundledBinaryExecutable(binary);
  assert.equal(fs.statSync(binary).mode & 0o777, 0o750);
});

function packagingFixture(t) {
  const root = temporaryDirectory(t);
  fs.mkdirSync(path.join(root, 'scripts'));
  const script = path.join(root, 'scripts', 'check-binaries.js');
  fs.copyFileSync(path.join(__dirname, '../scripts/check-binaries.js'), script);
  for (const target of ['win32-x64', 'darwin-x64', 'darwin-arm64']) {
    const directory = path.join(root, 'bin', target);
    fs.mkdirSync(directory, { recursive: true });
    for (const name of ['ql-engine', 'ql-lsp']) {
      fs.writeFileSync(path.join(directory, name + (target === 'win32-x64' ? '.exe' : '')), 'fixture', { mode: 0o644 });
    }
  }
  return { root, script };
}

test('prepublish restores both macOS architectures after artifact download', { skip: !posix }, t => {
  const { root, script } = packagingFixture(t);
  const result = spawnSync(process.execPath, [script], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  for (const target of ['darwin-x64', 'darwin-arm64']) {
    for (const name of ['ql-engine', 'ql-lsp']) {
      assert.equal(fs.statSync(path.join(root, 'bin', target, name)).mode & 0o777, 0o755);
    }
  }
  assert.equal(fs.statSync(path.join(root, 'bin/win32-x64/ql-engine.exe')).mode & 0o777, 0o644);
});

test('prepublish rejects an incomplete universal release', t => {
  const { root, script } = packagingFixture(t);
  fs.unlinkSync(path.join(root, 'bin/darwin-arm64/ql-lsp'));
  const result = spawnSync(process.execPath, [script], { encoding: 'utf8' });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /darwin-arm64[/\\]ql-lsp/);
});
