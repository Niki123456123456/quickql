const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const targets = [
  ['win32-x64', '.exe'],
  ['darwin-x64', ''],
  ['darwin-arm64', '']
];
const binaries = ['ql-engine', 'ql-lsp'];
const missing = [];

for (const [target, suffix] of targets) {
  for (const binary of binaries) {
    const file = path.join(root, 'bin', target, `${binary}${suffix}`);
    if (!fs.existsSync(file)) missing.push(path.relative(root, file));
  }
}

if (missing.length > 0) {
  throw new Error(`Universal VSIX is missing binaries:\n${missing.join('\n')}\nBuild the binaries on Windows x64, macOS Intel, and Apple Silicon runners first.`);
}
