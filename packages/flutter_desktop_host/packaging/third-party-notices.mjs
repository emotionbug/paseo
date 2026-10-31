import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const sha256 = value => createHash('sha256').update(value).digest('hex');
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const noticeName = /^(?:licen[cs]es?|copying|copyright|notices?|unlicense|authors|patents|third[-_ ]?party[-_ ]?(?:notices?|licenses?))(?:[._ -].*)?$/i;
const licenseName = /^(?:licen[cs]es?|copying|unlicense)(?:[._ -].*)?$/i;
export const nativeCargoTargets = {
  'linux-x64': 'x86_64-unknown-linux-gnu', 'linux-arm64': 'aarch64-unknown-linux-gnu',
  'darwin-x64': 'x86_64-apple-darwin', 'darwin-arm64': 'aarch64-apple-darwin',
  'win32-x64': 'x86_64-pc-windows-msvc', 'win32-arm64': 'aarch64-pc-windows-msvc',
};

function text(file) {
  const bytes = fs.readFileSync(file);
  const value = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  if (!value.trim() || value.includes('\0')) throw new Error(`Empty or non-text notice: ${file}`);
  return value;
}

// Scan the dependency itself, including vendored native subcomponents. Never
// traverse node_modules (each emitted package is attributed independently).
export function packageNoticeFiles(directory, licenseFile) {
  const files = new Set();
  function walk(current, inLicenses = false) {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      if (['node_modules', '.git', 'target'].includes(entry.name)) continue;
      const file = path.join(current, entry.name);
      if (entry.isDirectory()) walk(file, inLicenses || /^licen[cs]es$/i.test(entry.name));
      else if (noticeName.test(entry.name) || inLicenses) files.add(file);
    }
  }
  walk(directory);
  if (licenseFile) files.add(path.resolve(directory, licenseFile));
  return [...files].sort(compare).map(file => ({
    source: path.relative(directory, file).split(path.sep).join('/'), text: text(file),
    license: file === path.resolve(directory, licenseFile || '.') || licenseName.test(path.basename(file)) || /[/\\]licenses[/\\]/i.test(file),
  }));
}

export function noticeDocument(packages, boundary) {
  const texts = new Map();
  const records = packages.map(pkg => {
    if (!pkg.name || !pkg.version || !pkg.notices?.length) throw new Error(`Missing attribution for ${pkg.name || 'unnamed dependency'}`);
    const notices = pkg.notices.map(notice => {
      if (typeof notice.text !== 'string' || !notice.text.trim()) throw new Error(`Missing notice text for ${pkg.name}: ${notice.source}`);
      const digest = sha256(notice.text);
      texts.set(digest, notice.text);
      return { source: notice.source, sha256: digest };
    }).sort((a, b) => compare(a.source, b.source) || compare(a.sha256, b.sha256));
    const { notices: _, ...metadata } = pkg;
    return { ...metadata, notices };
  }).sort((a, b) => compare(`${a.name}@${a.version}:${a.source || ''}`, `${b.name}@${b.version}:${b.source || ''}`));
  return { schemaVersion: 1, boundary, packages: records, texts: Object.fromEntries([...texts].sort(([a], [b]) => compare(a, b))) };
}

export function renderNotices(document) {
  if (document.schemaVersion !== 1 || !Array.isArray(document.packages) || !document.packages.length || !document.texts) throw new Error('Malformed third-party notice document');
  const associations = new Map();
  for (const pkg of document.packages) {
    if (!pkg.name || !pkg.version || !pkg.notices?.length) throw new Error('Malformed third-party package attribution');
    for (const notice of pkg.notices) {
      const value = document.texts[notice.sha256];
      if (typeof value !== 'string' || !value.trim() || sha256(value) !== notice.sha256) throw new Error(`Missing or corrupt notice for ${pkg.name}: ${notice.source}`);
      if (!associations.has(notice.sha256)) associations.set(notice.sha256, []);
      associations.get(notice.sha256).push(`${pkg.name}@${pkg.version} (${pkg.license || 'see upstream text'}): ${notice.source}`);
    }
  }
  return `${document.boundary}\n\n` + [...associations].sort(([a], [b]) => compare(a, b)).map(([digest, owners]) =>
    `${'='.repeat(72)}\n${owners.sort(compare).join('\n')}\nSHA-256: ${digest}\n\n${document.texts[digest]}\n`).join('\n');
}

export function writeNotices(directory, document, basename = 'THIRD-PARTY-NOTICES') {
  const rendered = renderNotices(document);
  fs.mkdirSync(directory, { recursive: true });
  fs.writeFileSync(path.join(directory, `${basename}.json`), JSON.stringify(document, null, 2) + '\n');
  fs.writeFileSync(path.join(directory, `${basename}.txt`), rendered);
}

export function cargoMetadata({ cwd = root, target = nativeCargoTargets[`${process.platform}-${process.arch}`], cargo = process.env.CARGO || 'cargo' } = {}) {
  if (!target || !Object.values(nativeCargoTargets).includes(target)) throw new Error(`Unsupported native Cargo target: ${target}`);
  return JSON.parse(execFileSync(cargo, ['metadata', '--offline', '--locked', '--format-version', '1', '--filter-platform', target], {
    cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024,
  }));
}

export function collectCargoNotices(metadata, { supplements = JSON.parse(fs.readFileSync(new URL('./cargo-notice-supplements.json', import.meta.url), 'utf8')) } = {}) {
  const packages = new Map(metadata.packages.map(pkg => [pkg.id, pkg]));
  const nodes = new Map(metadata.resolve?.nodes?.map(node => [node.id, node]));
  const daemon = metadata.packages.find(pkg => pkg.name === 'paseo-daemon');
  if (!daemon || !nodes.has(daemon.id)) throw new Error('Cargo metadata lacks the resolved paseo-daemon graph');
  const visited = new Set();
  function visit(id) {
    if (visited.has(id)) return;
    if (!packages.has(id) || !nodes.has(id)) throw new Error(`Incomplete Cargo metadata: ${id}`);
    visited.add(id);
    for (const dep of nodes.get(id).deps) {
      if (!Array.isArray(dep.dep_kinds)) throw new Error(`Missing Cargo dependency kinds: ${id}`);
      if (dep.dep_kinds.some(kind => kind.kind !== 'dev')) visit(dep.pkg);
    }
  }
  visit(daemon.id);
  const result = [];
  for (const id of visited) {
    if (id === daemon.id) continue;
    const pkg = packages.get(id);
    const directory = path.dirname(pkg.manifest_path);
    const notices = packageNoticeFiles(directory, pkg.license_file);
    // Four published crates omit the repository LICENSE. These exact-version
    // supplements retain the upstream revision and verbatim fetched text.
    const supplement = supplements.find(entry => entry.name === pkg.name && entry.version === pkg.version && entry.repository === pkg.repository);
    if (supplement) {
      const vcs = JSON.parse(fs.readFileSync(path.join(directory, '.cargo_vcs_info.json'), 'utf8'));
      if (vcs.git?.sha1 !== supplement.revision || sha256(supplement.text) !== supplement.sha256) throw new Error(`Stale Cargo notice supplement: ${pkg.name}@${pkg.version}`);
      notices.push({ source: supplement.url, text: supplement.text, license: true });
    }
    if (!notices.some(notice => notice.license)) throw new Error(`No upstream license text for Cargo dependency ${pkg.name}@${pkg.version} (${pkg.license || 'undeclared'}); add a revision-pinned upstream supplement, not an SPDX template`);
    // SQLite's public-domain dedication is in the amalgamation, not a separate
    // license file. Keep that original header as well as the Rust wrapper license.
    if (pkg.name === 'libsqlite3-sys') {
      const source = text(path.join(directory, 'sqlite3/sqlite3.c'));
      const header = [...source.slice(0, 4096).matchAll(/\/\*[\s\S]*?\*\//g)].find(match => match[0].includes('disclaims copyright'))?.[0];
      if (!header?.includes('disclaims copyright')) throw new Error('Missing SQLite public-domain dedication');
      notices.push({ source: 'sqlite3/sqlite3.c (opening dedication)', text: header });
    }
    result.push({ name: pkg.name, version: pkg.version, source: pkg.source || 'path dependency', license: pkg.license, notices });
  }
  return noticeDocument(result, 'Rust daemon Cargo dependency closure (normal and build dependencies; dev-only edges excluded). Includes upstream license/notice files and nested vendored-component notices; this is not a source-offer or a toolchain/system-library compliance certification.');
}

export function collectWorkerNotices({ bundledInputs, compilerDirectories, cwd = root, compilerNotices = JSON.parse(fs.readFileSync(new URL('./esbuild-notice-sources.json', import.meta.url), 'utf8')) }) {
  if (!Array.isArray(bundledInputs) || !bundledInputs.length) throw new Error('Missing esbuild bundled-input manifest');
  const directories = new Set(compilerDirectories.map(directory => fs.realpathSync(directory)));
  for (const input of bundledInputs) {
    const file = path.resolve(cwd, input);
    if (!fs.statSync(file).isFile()) throw new Error(`Missing bundled input: ${input}`);
    // Workspace code is covered by the application's LICENSE. Only npm input
    // packages are third party, including nested workspace node_modules.
    if (!file.split(path.sep).includes('node_modules')) continue;
    let directory = path.dirname(file);
    while (!fs.existsSync(path.join(directory, 'package.json'))) {
      const parent = path.dirname(directory);
      if (parent === directory) throw new Error(`Cannot identify bundled npm input: ${input}`);
      directory = parent;
    }
    // Some packages have nested package.json files declaring only module type.
    while (!JSON.parse(fs.readFileSync(path.join(directory, 'package.json'), 'utf8')).name) {
      directory = path.dirname(directory);
      while (!fs.existsSync(path.join(directory, 'package.json'))) {
        const parent = path.dirname(directory);
        if (parent === directory) throw new Error(`Cannot identify bundled npm package: ${input}`);
        directory = parent;
      }
    }
    directories.add(fs.realpathSync(directory));
  }
  return noticeDocument([...directories].map(directory => {
    const pkg = JSON.parse(fs.readFileSync(path.join(directory, 'package.json'), 'utf8'));
    const notices = packageNoticeFiles(directory);
    if (/^@esbuild\/(?:linux|darwin|win32)-(?:x64|arm64)$/.test(pkg.name) && !notices.some(notice => notice.license)) {
      const owner = [...directories].find(candidate => {
        const metadata = JSON.parse(fs.readFileSync(path.join(candidate, 'package.json'), 'utf8'));
        return metadata.name === 'esbuild' && metadata.version === pkg.version && metadata.optionalDependencies?.[pkg.name] === pkg.version;
      });
      if (!owner) throw new Error(`No matching esbuild license owner for ${pkg.name}@${pkg.version}`);
      notices.push(...packageNoticeFiles(owner).map(notice => ({ ...notice, source: `esbuild@${pkg.version}/${notice.source}` })));
    }
    if (/^@esbuild\//.test(pkg.name)) {
      if (compilerNotices.schemaVersion !== 1 || compilerNotices.esbuildVersion !== pkg.version || !compilerNotices.packages?.length) throw new Error(`Missing pinned Go runtime notices for ${pkg.name}@${pkg.version}`);
      for (const dependency of compilerNotices.packages) {
        if (!dependency.name || !dependency.version || !dependency.notices?.length) throw new Error('Incomplete pinned compiler notice inventory');
        for (const notice of dependency.notices) {
          if (sha256(notice.text) !== notice.sha256) throw new Error(`Corrupt compiler notice: ${dependency.name}`);
          notices.push({ source: `${dependency.name}@${dependency.version}: ${notice.source}`, text: notice.text });
        }
      }
    }
    if (!notices.some(notice => notice.license)) throw new Error(`No upstream license text for bundled npm dependency ${pkg.name}@${pkg.version}`);
    return { name: pkg.name, version: pkg.version, license: typeof pkg.license === 'string' ? pkg.license : pkg.license?.type, notices };
  }), 'Optional external-Node plugin worker: every npm package in the esbuild input manifest, plus the copied esbuild, platform compiler and TypeScript packages. Includes pinned esbuild upstream Go/runtime/module notice files (conservative source inventory, not a per-object binary audit). Node itself is not bundled.');
}

export function dateParserNotice() {
  const source = fs.readFileSync(path.join(root, 'packages/daemon/src/agent/date_parse.rs'), 'utf8');
  const notice = source.match(/^(?:\/\/[^\n]*\n)+/)?.[0];
  if (!notice?.startsWith("// Native port of V8's DateParser") || !notice.includes('POSSIBILITY OF SUCH DAMAGE.')) throw new Error('The native DateParser port must retain its complete upstream copyright and BSD-3-Clause notice.');
  return notice;
}

export function collectSherpaNotices(assets) {
  const snapshot = JSON.parse(fs.readFileSync(new URL('./sherpa-notice-sources.json', import.meta.url), 'utf8'));
  if (snapshot.schemaVersion !== 1 || JSON.stringify(snapshot.assets) !== JSON.stringify(assets)) throw new Error('Sherpa release assets changed: refresh the pinned upstream notice inventory before packaging');
  for (const pkg of snapshot.packages) {
    for (const notice of pkg.notices) {
      if (sha256(notice.text) !== notice.sha256) throw new Error(`Corrupt speech notice: ${pkg.name} ${notice.source}`);
    }
  }
  const boundary = 'Pinned Sherpa ONNX 1.12.28 speech-library source/build notice inventory, including ONNX Runtime 1.23.2 upstream ThirdPartyNotices. Conservative coverage, not redistribution clearance.\n\nUNRESOLVED REDISTRIBUTION REQUIREMENTS:\n' + snapshot.unresolved.map(item => `- ${item}`).join('\n');
  return { ...noticeDocument(snapshot.packages, boundary), evidence: snapshot.evidence, unresolved: snapshot.unresolved };
}

export function nativeThirdPartyNotices(directory) {
  const cargo = JSON.parse(fs.readFileSync(path.join(directory, 'RUST-THIRD-PARTY-NOTICES.json'), 'utf8'));
  const worker = JSON.parse(fs.readFileSync(path.join(directory, 'plugin-worker/THIRD-PARTY-NOTICES.json'), 'utf8'));
  const speech = JSON.parse(fs.readFileSync(path.join(directory, 'lib/SPEECH-THIRD-PARTY-NOTICES.json'), 'utf8'));
  return dateParserNotice() + '\n' + renderNotices(cargo) + '\n' + renderNotices(worker) + '\n' + renderNotices(speech);
}
