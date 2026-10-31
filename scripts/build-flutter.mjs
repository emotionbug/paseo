import fs from 'node:fs';
import path from 'node:path';
import { root, app, release, version, platform, arch, flutter, buildNumber as resolveBuildNumber, run, capture, requireNode24, artifactStem, json } from '../packages/flutter_desktop_host/packaging/common.mjs';
import { bundleRuntime } from '../packages/flutter_desktop_host/packaging/runtime.mjs';

const args = process.argv.slice(2);
let target;
let mode = 'release';
let launch = false;
let device;
let androidArch = 'universal';
for (let index = 0; index < args.length; index++) {
  const argument = args[index];
  if (argument === '--debug') mode = 'debug';
  else if (argument === '--run') launch = true;
  else if (argument === '--device' && args[index + 1] && !args[index + 1].startsWith('--')) device = args[++index];
  else if (argument === '--arch' && args[index + 1]) androidArch = args[++index];
  else if (!argument.startsWith('--') && !target) target = argument;
  else throw new Error('Usage: node scripts/build-flutter.mjs [linux|macos|windows|android|ios] [--debug] [--run [--device ID]] [--arch universal|arm64]');
}
target ||= platform;
if (!['linux', 'macos', 'windows', 'android', 'ios'].includes(target)) throw new Error(`Unknown native platform: ${target}`);
if (['linux', 'macos', 'windows'].includes(target) && target !== platform) throw new Error('Desktop builds require a native host of the target platform.');
if (target === 'ios' && platform !== 'macos') throw new Error('iOS builds require macOS and Xcode.');
if (!['universal', 'arm64'].includes(androidArch) || (args.includes('--arch') && (target !== 'android' || launch))) throw new Error('--arch universal|arm64 is only supported for Android artifact builds, not --run.');
if (device && (!launch || !['android', 'ios'].includes(target))) throw new Error('--device requires --run on Android or iOS.');
if (['linux', 'macos', 'windows'].includes(target) && mode === 'debug' && process.env.PASEO_NATIVE_RELEASE === '1') throw new Error('PASEO_NATIVE_RELEASE=1 requires a release Flutter build.');
requireNode24();
const sdk = flutter();
if (launch) {
  process.env.PASEO_HOME = path.join(root, '.dev/paseo-home');
  process.env.PASEO_LISTEN ||= '127.0.0.1:6768';
  process.env.PASEO_NODE_ENV = 'development';
  fs.mkdirSync(process.env.PASEO_HOME, { recursive: true });
}
process.env.PERMISSION_HANDLER_INFO_PLIST ||= path.join(app, 'ios/Runner/Info.plist');
const restore = [];
function stage(source, destination) {
  if (!source) return;
  if (!fs.existsSync(source)) throw new Error(`Missing credential file: ${source}`);
  const before = fs.existsSync(destination) ? fs.readFileSync(destination) : null;
  restore.push(() => before === null ? fs.rmSync(destination, { force: true }) : fs.writeFileSync(destination, before));
  fs.copyFileSync(source, destination);
}
try {
  if (target === 'android' && process.env.GOOGLE_SERVICES_FILE_SESSIONS) {
    const source = process.env.GOOGLE_SERVICES_FILE_SESSIONS;
    const config = JSON.parse(fs.readFileSync(source, 'utf8'));
    if (!config.client?.some(client => client.client_info?.android_client_info?.package_name === 'sh.paseo.sessions')) throw new Error('Firebase configuration is not for sh.paseo.sessions.');
    stage(source, path.join(app, 'android/app/google-services.json'));
  }
  if (target === 'ios' && process.env.GOOGLE_SERVICE_INFO_PLIST_SESSIONS) {
    const source = path.resolve(process.env.GOOGLE_SERVICE_INFO_PLIST_SESSIONS);
    process.env.GOOGLE_SERVICE_INFO_PLIST_SESSIONS = source;
    const bundleId = capture('/usr/libexec/PlistBuddy', ['-c', 'Print :BUNDLE_ID', source]);
    if (bundleId !== 'sh.paseo.sessions') throw new Error('Firebase configuration is not for sh.paseo.sessions.');
    stage(source, path.join(app, 'ios/Runner/GoogleService-Info.plist'));
  }
  run(sdk, ['pub', 'get', '--enforce-lockfile'], { cwd: app });
  const desktop = ['linux', 'macos', 'windows'].includes(target);
  if (desktop) {
    run(process.execPath, ['scripts/build-native-daemon.mjs', ...(mode === 'release' ? ['--release'] : [])]);
  }
  const buildNumber = resolveBuildNumber(target);
  const channel = process.env.PASEO_RELEASE_CHANNEL || (version.includes('-') ? 'beta' : 'stable');
  if (!['stable', 'beta'].includes(channel)) throw new Error('PASEO_RELEASE_CHANNEL must be stable or beta.');
  const options = [`--${mode}`, `--build-name=${version}`, `--build-number=${buildNumber}`,
    `--dart-define=PASEO_VERSION=${version}`, `--dart-define=PASEO_RELEASE_CHANNEL=${channel}`];
  if (launch && !desktop) {
    const devices = JSON.parse(capture(sdk, ['devices', '--machine'], { cwd: app }));
    const matching = devices.filter(candidate => candidate.isSupported !== false &&
      (target === 'android' ? candidate.targetPlatform?.startsWith('android-') : candidate.targetPlatform === 'ios'));
    const selected = device ? matching.find(candidate => candidate.id === device) : matching.length === 1 ? matching[0] : null;
    if (!selected) throw new Error(`Select an attached ${target} device with --device ID. Available: ${matching.map(candidate => `${candidate.name} (${candidate.id})`).join(', ') || 'none'}`);
    run(sdk, ['run', '-d', selected.id, ...options], { cwd: app });
  } else {
  let command = target;
  if (target === 'android') {
    command = 'apk';
    if (androidArch === 'arm64') options.push('--target-platform=android-arm64');
  }
  if (target === 'ios') {
    if (process.env.PASEO_IOS_EXPORT_OPTIONS_PLIST) {
      command = 'ipa';
      options.push(`--export-options-plist=${path.resolve(process.env.PASEO_IOS_EXPORT_OPTIONS_PLIST)}`);
    } else {
      options.push('--no-codesign');
      console.warn('No iOS export/provisioning configuration: building an explicitly unsigned device .app, not an installable IPA.');
    }
  }
  run(sdk, ['build', command, ...options], { cwd: app });
  fs.mkdirSync(release, { recursive: true });
  let output;
  if (desktop) {
    output = target === 'linux' ? path.join(app, `build/linux/${arch}/${mode}/bundle`) :
      target === 'windows' ? path.join(app, `build/windows/${arch}/runner/${mode === 'release' ? 'Release' : 'Debug'}`) :
      path.join(app, `build/macos/Build/Products/${mode === 'release' ? 'Release' : 'Debug'}/Paseo Sessions.app`);
    if (!fs.existsSync(output)) throw new Error(`Native build output missing: ${output}`);
    await bundleRuntime(path.join(output, target === 'macos' ? 'Contents/Resources/runtime' : 'runtime'), { profile: mode });
    json(path.join(release, `build-${target}-${arch}.json`), { version, platform: target, arch, mode, output: path.relative(root, output) });
    if (launch) {
      const executable = target === 'macos' ? path.join(output, 'Contents/MacOS/Paseo Sessions') : path.join(output, target === 'windows' ? 'paseo_sessions.exe' : 'paseo_sessions');
      run(executable, [], { cwd: output });
    }
  } else if (target === 'android') {
    const propertiesFile = path.join(app, 'android/key.properties');
    const signingProperties = fs.existsSync(propertiesFile) ? fs.readFileSync(propertiesFile, 'utf8') : '';
    const releaseKey = process.env.ANDROID_KEYSTORE_PATH?.trim() || /^\s*storeFile\s*(?:=|:|\s)\s*\S/m.test(signingProperties);
    const signing = mode === 'debug' || !releaseKey ? '-debug-signed' : '';
    output = path.join(release, `${artifactStem('android', androidArch)}${signing}.apk`);
    fs.copyFileSync(path.join(app, `build/app/outputs/flutter-apk/app-${mode}.apk`), output);
  } else if (command === 'ipa') {
    const ipas = fs.readdirSync(path.join(app, 'build/ios/ipa')).filter(file => file.endsWith('.ipa'));
    if (ipas.length !== 1) throw new Error('Expected exactly one exported IPA.');
    output = path.join(release, `${artifactStem('ios', 'arm64')}.ipa`);
    fs.copyFileSync(path.join(app, 'build/ios/ipa', ipas[0]), output);
  } else {
    output = path.join(release, `${artifactStem('ios', 'arm64')}-unsigned.app.zip`);
    run('ditto', ['-c', '-k', '--sequesterRsrc', '--keepParent', path.join(app, 'build/ios/iphoneos/Runner.app'), output]);
  }
  console.log(`Flutter ${target} ${mode}: ${output}`);
  }
} finally {
  for (const cleanup of restore.reverse()) cleanup();
}
