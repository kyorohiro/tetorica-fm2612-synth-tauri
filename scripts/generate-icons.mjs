import { mkdtempSync, copyFileSync, mkdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';

// Keep desktop outputs only. The source is the upstream Tetorica icon.
const temporary = mkdtempSync(join(tmpdir(), 'tetorica-icons-'));
try {
  execFileSync(process.platform === 'win32' ? 'tauri.cmd' : 'tauri',
    ['icon', 'assets/app-icon.png', '--output', temporary], { stdio: 'inherit' });
  mkdirSync('src-tauri/icons', { recursive: true });
  for (const name of ['icon.png', 'icon.icns', 'icon.ico', '32x32.png', '128x128.png', '128x128@2x.png']) {
    copyFileSync(join(temporary, name), join('src-tauri/icons', name));
  }
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
