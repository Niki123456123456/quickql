import * as fs from 'fs';

/** Repair permissions stripped from bundled binaries by artifact/VSIX extraction. */
export function ensureBundledBinaryExecutable(binary: string): void {
  if (process.platform === 'win32') {
    return;
  }

  const mode = fs.statSync(binary).mode;
  if ((mode & 0o100) === 0) {
    fs.chmodSync(binary, mode | 0o100);
  }
  fs.accessSync(binary, fs.constants.X_OK);
}
