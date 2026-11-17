import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

export function releaseWorkspace(cwd, commit) {
  const parent = mkdtempSync(join(tmpdir(), 'auram-release-'));
  const directory = join(parent, 'source');
  try {
    execFileSync('git', ['worktree', 'add', '--detach', directory, commit], { cwd, stdio: 'inherit' });
  } catch (error) {
    rmSync(parent, { recursive: true, force: true });
    throw error;
  }
  return { directory, close() {
    execFileSync('git', ['worktree', 'remove', '--force', directory], { cwd, stdio: 'inherit' });
    rmSync(parent, { recursive: true, force: true });
  } };
}

export function signingEnvironment(cwd, environment = process.env) {
  const configFile = join(cwd, '.auram-release.local.json');
  const config = existsSync(configFile) ? JSON.parse(readFileSync(configFile, 'utf8')) : {};
  const result = { ...environment, CI: 'true', CARGO_TARGET_DIR: resolve(cwd, 'src-tauri/target') };
  const keyFile = environment.AURAM_UPDATER_PRIVATE_KEY_FILE || config.updaterKeyFile;
  if (!result.TAURI_SIGNING_PRIVATE_KEY && keyFile) {
    result.TAURI_SIGNING_PRIVATE_KEY = readFileSync(resolve(cwd, keyFile), 'utf8');
  }
  result.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ??= '';
  return result;
}

export function buildMacLocally(directory, cwd) {
  const env = signingEnvironment(cwd);
  const options = { cwd: directory, env, stdio: 'inherit' };
  execFileSync('pnpm', ['install', '--frozen-lockfile'], options);
  execFileSync('pnpm', ['build', '--target', 'aarch64-apple-darwin', '--bundles', 'dmg,app'], options);
  execFileSync(process.execPath, ['scripts/collect-artifacts.mjs', 'aarch64-apple-darwin', 'darwin-aarch64'], options);
}
