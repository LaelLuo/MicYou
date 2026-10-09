import { stat } from 'node:fs/promises';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

export const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const identity = 'com.laelluo.micyou.duplex';
export const receiptPath = join(repo, 'tauri-app/target/duplex-build.json');
export const binaries = [
  'tauri-app/target/x86_64-pc-windows-msvc/release/micyou.exe',
  'tauri-app/src-tauri/binaries/micyou-cli-x86_64-pc-windows-msvc.exe',
  'tauri-app/src-tauri/binaries/micyou-tui-x86_64-pc-windows-msvc.exe',
  'composeApp/build/outputs/apk/debug/composeApp-debug.apk',
];

export function readCommand(args: string[], cwd = repo): string {
  const result = Bun.spawnSync(args, { cwd, env: process.env });
  if (result.exitCode !== 0) throw new Error(`${args[0]} failed: ${result.stderr.toString()}`);
  return result.stdout.toString();
}
export function cleanRevision(): string {
  if (readCommand(['git', 'status', '--porcelain']).trim()) {
    throw new Error('Commit reviewed source before building or packaging.');
  }
  return readCommand(['git', 'rev-parse', 'HEAD']).trim();
}
export async function hashFile(path: string): Promise<string> {
  if (!(await stat(path)).isFile()) throw new Error(`Not a file: ${path}`);
  const bytes = await Bun.file(path).arrayBuffer();
  if (!bytes.byteLength) throw new Error(`Empty artifact: ${path}`);
  return new Bun.CryptoHasher('sha256').update(bytes).digest('hex');
}
export async function binaryHashes(): Promise<Record<string, string>> {
  const hashes: Record<string, string> = {};
  for (const source of binaries) hashes[source] = await hashFile(join(repo, source));
  return hashes;
}
export function inspectApk() {
  const sdk = process.env.ANDROID_HOME || process.env.ANDROID_SDK_ROOT;
  const jdk = process.env.JAVA_HOME;
  if (!sdk || !jdk) throw new Error('Set ANDROID_HOME and JAVA_HOME for APK identity verification.');
  const tools = join(sdk, 'build-tools/36.1.0');
  const apk = join(repo, binaries[3]);
  const badging = readCommand([join(tools, 'aapt2.exe'), 'dump', 'badging', apk]);
  const packageLine = badging.match(/^package: name='([^']+)' versionCode='([^']+)' versionName='([^']*)'/m);
  if (!packageLine || packageLine[1] !== identity) {
    throw new Error(`Refusing APK with unexpected identity: ${packageLine?.[1] ?? 'unknown'}`);
  }
  const signature = readCommand([join(jdk, 'bin/java.exe'), '-jar', join(tools, 'lib/apksigner.jar'),
    'verify', '--print-certs', apk]);
  const signerSha256 = signature.match(/^Signer #1 certificate SHA-256 digest: ([a-f0-9]+)$/im)?.[1];
  if (!signerSha256) throw new Error('Cannot verify APK signer SHA256.');
  return { applicationId: packageLine[1], versionCode: packageLine[2], versionName: packageLine[3], signerSha256 };
}
