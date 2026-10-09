import { mkdir, readdir, copyFile, stat } from 'node:fs/promises';
import { join, resolve, dirname } from 'node:path';
import { repo, identity, receiptPath, cleanRevision, hashFile, binaryHashes, inspectApk } from './duplex-artifacts';

// Package already-validated builds without launching either audio application.
const outputArg = process.argv[2];
if (!outputArg) throw new Error('Usage: bun scripts/package-duplex.ts <new-output-directory>');
const output = resolve(outputArg);
const existing = await readdir(output).catch((error: NodeJS.ErrnoException) => {
  if (error.code === 'ENOENT') return [];
  throw error;
});
if (existing.length) throw new Error('Output directory must be empty; existing packages are preserved.');

const files: [string, string][] = [
  ['tauri-app/target/x86_64-pc-windows-msvc/release/micyou.exe', 'windows/micyou.exe'],
  ['tauri-app/src-tauri/binaries/micyou-cli-x86_64-pc-windows-msvc.exe', 'windows/micyou-cli.exe'],
  ['tauri-app/src-tauri/binaries/micyou-tui-x86_64-pc-windows-msvc.exe', 'windows/micyou-tui.exe'],
  ['scripts/Start-MicYou-Duplex.ps1', 'windows/Start-MicYou-Duplex.ps1'],
  ['LICENSE', 'windows/LICENSE'],
  ['docs/duplex-audio.md', 'windows/README.md'],
  ['tauri-app/src-tauri/resources/LICENSE-AEC7.txt', 'windows/resources/LICENSE-AEC7.txt'],
  ['tauri-app/src-tauri/resources/LICENSE-PureVox.txt', 'windows/resources/LICENSE-PureVox.txt'],
  ['composeApp/build/outputs/apk/debug/composeApp-debug.apk', 'android/MicYou-Duplex-debug.apk'],
];
for (const [source] of files) {
  const info = await stat(join(repo, source));
  if (!info.isFile() || !info.size) throw new Error(`Missing or empty build input: ${source}`);
}
const revision = cleanRevision();
const receipt = await Bun.file(receiptPath).json();
if (receipt.revision !== revision || receipt.desktopIdentifier !== identity) {
  throw new Error('Build receipt does not match this source revision and Duplex identity.');
}
const actualHashes = await binaryHashes();
for (const [path, hash] of Object.entries(actualHashes)) {
  if (receipt.hashes[path] !== hash) throw new Error(`Artifact changed since verified build: ${path}`);
}
const android = inspectApk();
if (JSON.stringify(receipt.android) !== JSON.stringify(android)) throw new Error('APK identity changed since build.');

const hashes: Record<string, { sha256: string; bytes: number }> = {};
for (const [source, destination] of files) {
  const target = join(output, destination);
  await mkdir(dirname(target), { recursive: true });
  await copyFile(join(repo, source), target);
  const bytes = await Bun.file(target).arrayBuffer();
  hashes[destination] = { sha256: await hashFile(target), bytes: bytes.byteLength };
}
await copyFile(receiptPath, join(output, 'build-receipt.json'));
await Bun.write(join(output, 'manifest.json'), JSON.stringify({
  source: 'https://github.com/LaelLuo/MicYou',
  revision,
  createdAt: new Date().toISOString(),
  android,
  desktopIdentifier: identity,
  files: hashes,
}, null, 2) + '\n');
console.log(`Packaged ${files.length} files in ${output}`);
