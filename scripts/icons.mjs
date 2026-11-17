import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';

const require = createRequire(import.meta.url);
const cli = join(dirname(require.resolve('@tauri-apps/cli')), 'tauri.js');
const output = mkdtempSync(join(tmpdir(), 'auram-icons-'));
try {
  execFileSync(process.execPath, [cli, 'icon', 'assets/app-icon.svg', '--output', output], { stdio: 'inherit' });
  for (const name of readdirSync(output)) {
    if (/\.(png|icns|ico)$/.test(name)) copyFileSync(join(output, name), join('src-tauri/icons', name));
  }
} finally {
  rmSync(output, { recursive: true, force: true });
}
