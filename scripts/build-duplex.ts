import { join } from 'node:path';
import { repo, identity, receiptPath, cleanRevision, binaryHashes, inspectApk } from './duplex-artifacts';

if (process.platform !== 'win32') throw new Error('This delivery build targets Windows and Android.');
const revision = cleanRevision();
const jdk = process.env.JAVA_HOME;
if (!jdk) throw new Error('Set JAVA_HOME to JDK21 for the build process.');
const config = await Bun.file(join(repo, 'tauri-app/src-tauri/tauri.duplex.conf.json')).json();
if (config.identifier !== identity) throw new Error('Unexpected desktop Duplex identifier.');
const env: NodeJS.ProcessEnv = {
  ...process.env, RUSTUP_TOOLCHAIN: '1.95.0',
  CARGO_TARGET_DIR: join(repo, 'tauri-app/target'),
  CARGO_BUILD_TARGET: 'x86_64-pc-windows-msvc',
};
delete env.TAURI_CONFIG;
delete env.TAURI_ENV_TARGET_TRIPLE;
const commands = [
  { cwd: repo, args: [join(jdk, 'bin/java.exe'), '-Xmx64m', '-Xms64m', '-classpath',
    'gradle/wrapper/gradle-wrapper.jar', 'org.gradle.wrapper.GradleWrapperMain',
    '-Pmicyou.duplex=true', ':composeApp:testDebugUnitTest', ':composeApp:assembleDebug', '--console=plain'] },
  { cwd: join(repo, 'tauri-app'), args: [process.execPath, 'run-tauri.js', 'build', '--no-bundle',
    '--config', 'src-tauri/tauri.duplex.conf.json', '--target', 'x86_64-pc-windows-msvc'] },
];
for (const command of commands) {
  const child = Bun.spawn(command.args, { cwd: command.cwd, env, stdout: 'inherit', stderr: 'inherit', stdin: 'ignore' });
  const code = await child.exited;
  if (code !== 0) throw new Error(`Build failed (${code}); no new build receipt written.`);
}
if (cleanRevision() !== revision) throw new Error('Source revision changed during build.');
const receipt = {
  revision, createdAt: new Date().toISOString(), desktopIdentifier: config.identifier,
  android: inspectApk(), commands, hashes: await binaryHashes(),
};
await Bun.write(receiptPath, JSON.stringify(receipt, null, 2) + '\n');
console.log(`Verified Duplex build receipt: ${receiptPath}`);
