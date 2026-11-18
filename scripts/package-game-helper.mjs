import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

if (process.platform === 'darwin') {
  const file = (relative) => fileURLToPath(new URL(relative, import.meta.url));
  execFileSync('/bin/sh', [
    file('../src-tauri/native/macos/build-helper.sh'),
    file('../.build/mac-game-helper/Auram.app'),
    process.env.APPLE_SIGNING_IDENTITY || '-',
  ], { stdio: 'inherit' });
}
