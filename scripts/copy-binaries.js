const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const binDir = path.join(root, 'bin');
const suffix = process.platform === 'win32' ? '.exe' : '';
const binaries = ['ql-engine', 'ql-lsp'];

fs.mkdirSync(binDir, { recursive: true });

for (const binary of binaries) {
  const fileName = `${binary}${suffix}`;
  const source = path.join(root, 'target', 'release', fileName);
  const destination = path.join(binDir, fileName);

  if (!fs.existsSync(source)) {
    throw new Error(`Missing ${source}. Run cargo build --release first.`);
  }

  fs.copyFileSync(source, destination);

  if (process.platform !== 'win32') {
    fs.chmodSync(destination, 0o755);
  }
}
