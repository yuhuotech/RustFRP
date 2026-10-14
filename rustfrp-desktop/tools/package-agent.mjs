import { spawn } from 'node:child_process';
import { createReadStream } from 'node:fs';
import { copyFile, mkdir, rm, stat, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = resolve(desktop, '..');
function run(executable, args, cwd) {
  return new Promise((done, fail) => {
    const child = spawn(executable, args, { cwd, stdio: 'inherit', shell: false });
    child.once('error', fail); child.once('exit', (code, signal) => code === 0 ? done() : fail(new Error(`${executable} failed (${signal ?? code})`)));
  });
}
const target = process.env.TAURI_ENV_TARGET_TRIPLE || process.env.CARGO_BUILD_TARGET;
if (target && !/^[a-zA-Z0-9_.-]+$/.test(target)) throw new Error('Invalid Agent target');
const npm = process.platform === 'win32' ? process.execPath : 'npm';
const npmArgs = process.platform === 'win32' ? [process.env.npm_execpath || join(dirname(process.execPath), 'node_modules', 'npm', 'bin', 'npm-cli.js')] : [];
for (const command of ['check', 'build', 'bench']) await run(npm, [...npmArgs, 'run', command], join(root, 'rustfrp-ui'));
await run(process.env.CARGO || 'cargo', ['build', '--locked', '--release', '-p', 'rustfrp-agent', ...(target ? ['--target', target] : [])], root);
const windows = target ? target.includes('windows') : process.platform === 'win32';
const binary = `rustfrp-agent${windows ? '.exe' : ''}`;
const configuredTarget = process.env.CARGO_TARGET_DIR;
const targetDirectory = configuredTarget ? (isAbsolute(configuredTarget) ? configuredTarget : resolve(root, configuredTarget)) : join(root, 'target');
const source = join(targetDirectory, ...(target ? [target] : []), 'release', binary);
const metadata = await stat(source);
if (!metadata.isFile() || metadata.size === 0 || metadata.size > 32 * 1024 * 1024) throw new Error('Agent artifact is empty or exceeds 32 MiB');
const hash = createHash('sha256'); for await (const part of createReadStream(source)) hash.update(part);
const staged = join(desktop, 'resources', 'bin'); await rm(staged, { recursive: true, force: true }); await mkdir(staged, { recursive: true });
await copyFile(source, join(staged, binary));
await writeFile(join(staged, 'agent-manifest.json'), JSON.stringify({ version: 1, target: target ?? `${process.arch}-${process.platform}`, file: binary, bytes: metadata.size, sha256: hash.digest('hex') }) + '\n');
console.log(`Bundled ${binary}: ${metadata.size} bytes`);
