import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const watch = args[0] === '--watch';
if (watch) args.shift();
const cli = args[0] === '--';
if (cli) args.shift();
const command = cli || args.length ? args : ['daemon', 'run'];
if (watch && args.length) throw new Error('--watch only supports the foreground development daemon.');
const env = { ...process.env, PASEO_NODE_ENV: 'development' };
env.PATH = `${path.join(root, 'node_modules', '.bin')}${path.delimiter}${env.PATH || ''}`;
// Development builds must never inherit an updater-enabled release identity.
delete env.PASEO_NATIVE_RELEASE;
delete env.PASEO_UPDATE_REPOSITORY;
delete env.PASEO_RELEASE_CHANNEL;
const managed = !env.PASEO_HOME || env.PASEO_DEV_MANAGED_HOME === '1';
env.PASEO_HOME ||= path.join(env.PASEO_DEV_ROOT || root, '.dev', 'paseo-home');
env.PASEO_HOME = path.resolve(env.PASEO_HOME);
env.PASEO_LISTEN ||= env.PASEO_SERVICE_DAEMON_PORT ? `0.0.0.0:${env.PASEO_SERVICE_DAEMON_PORT}` : '127.0.0.1:6768';
env.PASEO_CORS_ORIGINS ||= '*';
env.PASEO_LOCAL_MODELS_DIR ||= path.join(os.homedir(), '.paseo', 'models', 'local-speech');
if (env.PASEO_DEV_SEED_HOME) {
  const source = path.resolve(env.PASEO_DEV_SEED_HOME);
  const sameHome = source === env.PASEO_HOME || (fs.existsSync(source) && fs.existsSync(env.PASEO_HOME) && fs.realpathSync(source) === fs.realpathSync(env.PASEO_HOME));
  if (!sameHome && fs.existsSync(source)) {
    if (env.PASEO_DEV_RESET_HOME === '1') fs.rmSync(env.PASEO_HOME, { recursive: true, force: true });
    if (!fs.existsSync(env.PASEO_HOME) || fs.readdirSync(env.PASEO_HOME).length === 0) {
      fs.mkdirSync(env.PASEO_HOME, { recursive: true });
      for (const member of ['agents', 'projects']) {
        const input = path.join(source, member);
        if (fs.existsSync(input)) fs.cpSync(input, path.join(env.PASEO_HOME, member), {
          recursive: true, filter: file => fs.statSync(file).isDirectory() || file.endsWith('.json'),
        });
      }
      if (fs.existsSync(path.join(source, 'config.json'))) fs.copyFileSync(path.join(source, 'config.json'), path.join(env.PASEO_HOME, 'config.json'));
    }
  }
}
fs.mkdirSync(env.PASEO_HOME, { recursive: true });
fs.mkdirSync(env.PASEO_LOCAL_MODELS_DIR, { recursive: true });
if (managed || env.PASEO_DEV_SEED_HOME) {
  const filename = path.join(env.PASEO_HOME, 'config.json');
  const config = fs.existsSync(filename) ? JSON.parse(fs.readFileSync(filename, 'utf8')) : {};
  config.version ||= 1;
  config.daemon ||= {};
  config.daemon.listen = env.PASEO_LISTEN;
  config.daemon.cors ||= {};
  config.daemon.cors.allowedOrigins = ['*'];
  fs.writeFileSync(filename, `${JSON.stringify(config, null, 2)}\n`);
}
const target = path.resolve(root, env.CARGO_TARGET_DIR || 'target', 'debug');
const executable = path.join(target, process.platform === 'win32' ? 'paseo.exe' : 'paseo');
console.log(`Paseo native development: home=${env.PASEO_HOME} listen=${env.PASEO_LISTEN}`);
let child;
let completion;
let stopping = false;
let restarting = false;
let pending = false;
let timer;
const watchers = [];

function start() {
  if (stopping) return;
  child = spawn(executable, command, { cwd: root, env, stdio: 'inherit' });
  completion = new Promise(resolve => {
    child.once('error', error => { console.error(error); resolve(1); });
    child.once('exit', code => resolve(code ?? 1));
  });
  completion.then(code => {
    child = undefined;
    if (stopping || restarting) return;
    if (code === 75) start();
    else if (!watch) process.exitCode = code;
    else console.error(`Native daemon exited (${code}); watching for source changes.`);
  });
}

async function stopChild() {
  if (!child) return;
  child.kill('SIGTERM');
  await completion;
}

async function rebuild() {
  if (stopping) return;
  if (restarting) { pending = true; return; }
  restarting = true;
  do {
    pending = false;
    await stopChild();
    if (stopping) break;
    const status = await new Promise(resolve => {
      const building = spawn(process.execPath, [path.join(root, 'scripts/build-native-daemon.mjs')], { cwd: root, env, stdio: 'inherit' });
      building.once('error', error => { console.error(error); resolve(1); });
      building.once('exit', code => resolve(code ?? 1));
    });
    if (!pending && !stopping) {
      if (status === 0) start();
      else if (!watch) process.exitCode = status;
      else console.error('Native build failed; watching for source changes.');
    }
  } while (pending && !stopping);
  restarting = false;
}

async function shutdown(signal) {
  if (stopping) return;
  stopping = true;
  clearTimeout(timer);
  for (const watcher of watchers) watcher.close();
  // Let an in-flight build settle: killing only its Node wrapper could orphan Cargo.
  await stopChild();
  process.exitCode = signal === 'SIGINT' ? 130 : 143;
}
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => { void shutdown(signal); });
if (watch) {
  const changed = (_event, filename) => {
    if (filename && /(?:^|[/\\])(?:dist|target|node_modules)(?:[/\\]|$)/.test(String(filename))) return;
    clearTimeout(timer);
    timer = setTimeout(() => { void rebuild(); }, 200);
  };
  for (const member of ['packages/daemon', 'packages/plugin/src', 'packages/protocol/src', 'plugins', 'skills']) {
    watchers.push(fs.watch(path.join(root, member), { recursive: true }, changed));
  }
  watchers.push(fs.watch(root, (_event, filename) => {
    if (['Cargo.toml', 'Cargo.lock'].includes(String(filename))) changed();
  }));
}
if (env.PASEO_SKIP_NATIVE_BUILD === '1') {
  if (!fs.existsSync(executable)) throw new Error(`No native development build at ${executable}; run npm run build:daemon first.`);
  start();
} else await rebuild();
