const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const platform = process.platform;
const arch = process.arch;
const platformDir = path.join(root, 'bin', `${platform}-${arch}`);
const suffix = process.platform === 'win32' ? '.exe' : '';
const binaries = ['ql-engine', 'ql-lsp'];

fs.mkdirSync(platformDir, { recursive: true });

for (const binary of binaries) {
  const fileName = `${binary}${suffix}`;
  const source = path.join(root, 'target', 'release', fileName);
  const destination = path.join(platformDir, fileName);

  if (!fs.existsSync(source)) {
    throw new Error(`Missing ${source}. Run cargo build --release first.`);
  }

  fs.copyFileSync(source, destination);

  if (process.platform !== 'win32') {
    fs.chmodSync(destination, 0o755);
  }
}
