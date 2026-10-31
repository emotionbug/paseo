import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import {
  collectCargoNotices, collectWorkerNotices, collectSherpaNotices,
  noticeDocument, renderNotices,
} from '../packages/flutter_desktop_host/packaging/third-party-notices.mjs';
import sherpaAssets from '../packages/flutter_desktop_host/packaging/sherpa-assets.json' with { type: 'json' };

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'paseo-notices-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  function file(relative, value) {
    const target = path.join(directory, relative);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, value);
    return target;
  }
  function pkg(name, notices = { LICENSE: 'Fixture license\n' }) {
    const manifest_path = file(`${name}/Cargo.toml`, `[package]\nname = "${name}"\nversion = "1.0.0"\n`);
    for (const [relative, value] of Object.entries(notices)) file(`${name}/${relative}`, value);
    return { id: name, name, version: '1.0.0', license: 'fixture', manifest_path, source: 'fixture' };
  }
  return { directory, file, pkg };
}
function graph(packages, dependencies) {
  return { packages, resolve: { nodes: packages.map(pkg => ({ id: pkg.id, deps: dependencies[pkg.id] || [] })) } };
}
const edge = (pkg, kind = null) => ({ pkg, dep_kinds: [{ kind, target: null }] });

test('Cargo graph preserves normal/build and nested notices, excludes dev-only dependencies, and deduplicates texts', t => {
  const f = fixture(t);
  const packages = [f.pkg('paseo-daemon'), f.pkg('normal', { LICENSE: 'same license\n', 'vendor/NOTICE': 'native notice\n' }), f.pkg('build', { 'LICENSE-MIT': 'same license\n' }), f.pkg('dev', {})];
  const metadata = graph(packages, { 'paseo-daemon': [edge('normal'), edge('build', 'build'), edge('dev', 'dev')] });
  const document = collectCargoNotices(metadata, { supplements: [] });
  assert.deepEqual(document.packages.map(pkg => pkg.name), ['build', 'normal']);
  assert.equal(Object.keys(document.texts).length, 2);
  assert.equal(document.packages.find(pkg => pkg.name === 'normal').notices.length, 2);
  const reversed = { ...metadata, packages: [...metadata.packages].reverse(), resolve: { nodes: [...metadata.resolve.nodes].reverse() } };
  assert.deepEqual(collectCargoNotices(reversed, { supplements: [] }), document);
});

test('missing, empty, malformed and unresolved Cargo notice inputs fail closed', t => {
  const f = fixture(t);
  const daemon = f.pkg('paseo-daemon');
  const missing = f.pkg('missing', {});
  const metadata = graph([daemon, missing], { 'paseo-daemon': [edge('missing')] });
  assert.throws(() => collectCargoNotices(metadata, { supplements: [] }), /No upstream license/);
  f.file('missing/LICENSE', '');
  assert.throws(() => collectCargoNotices(metadata, { supplements: [] }), /Empty or non-text/);
  f.file('missing/LICENSE', Buffer.from([0xff]));
  assert.throws(() => collectCargoNotices(metadata, { supplements: [] }), /encoded data|encoding/i);
  f.file('missing/LICENSE', 'valid fixture\n');
  missing.license_file = 'missing-custom-license';
  assert.throws(() => collectCargoNotices(metadata, { supplements: [] }), /ENOENT/);
  delete missing.license_file;
  metadata.resolve.nodes = metadata.resolve.nodes.filter(node => node.id !== 'missing');
  assert.throws(() => collectCargoNotices(metadata, { supplements: [] }), /Incomplete Cargo metadata/);
});

test('supplements are exact-version and source-revision bound, not fallback SPDX templates', t => {
  const f = fixture(t);
  const dep = { ...f.pkg('dep', {}), repository: 'https://example.invalid/dep' };
  const metadata = graph([f.pkg('paseo-daemon'), dep], { 'paseo-daemon': [edge('dep')] });
  f.file('dep/.cargo_vcs_info.json', JSON.stringify({ git: { sha1: 'actual-revision' } }));
  const supplement = { name: 'dep', version: '1.0.0', repository: dep.repository, revision: 'other-revision', text: 'fixture', sha256: 'invalid', url: 'fixture' };
  assert.throws(() => collectCargoNotices(metadata, { supplements: [supplement] }), /Stale Cargo notice supplement/);
  supplement.version = '2.0.0';
  assert.throws(() => collectCargoNotices(metadata, { supplements: [supplement] }), /No upstream license/);
});

test('worker attributes nested npm inputs and matching compiler binary without traversing unrelated node_modules', t => {
  const f = fixture(t);
  f.file('workspace/source.ts', 'export {};');
  f.file('node_modules/dep/package.json', JSON.stringify({ name: 'dep', version: '1', license: 'fixture' }));
  f.file('node_modules/dep/lib/package.json', JSON.stringify({ type: 'module' }));
  f.file('node_modules/dep/lib/index.js', 'export {};');
  f.file('node_modules/dep/LICENSE', 'dep license\n');
  f.file('node_modules/unused/package.json', JSON.stringify({ name: 'unused', version: '1' }));
  const compiler = path.dirname(f.file('node_modules/esbuild/package.json', JSON.stringify({ name: 'esbuild', version: '1', optionalDependencies: { '@esbuild/linux-x64': '1' } })));
  f.file('node_modules/esbuild/LICENSE', 'esbuild fixture license\n');
  const binary = path.dirname(f.file('node_modules/@esbuild/linux-x64/package.json', JSON.stringify({ name: '@esbuild/linux-x64', version: '1' })));
  const runtimeText = 'esbuild fixture license\n';
  const compilerNotices = { schemaVersion: 1, esbuildVersion: '1', packages: [{ name: 'fixture-runtime', version: '1', notices: [{ source: 'LICENSE', text: runtimeText, sha256: createHash('sha256').update(runtimeText).digest('hex') }] }] };
  const options = { cwd: f.directory, bundledInputs: ['workspace/source.ts', 'node_modules/dep/lib/index.js'], compilerDirectories: [compiler, binary], compilerNotices };
  const document = collectWorkerNotices(options);
  assert.deepEqual(document.packages.map(pkg => pkg.name), ['@esbuild/linux-x64', 'dep', 'esbuild']);
  assert.equal(Object.keys(document.texts).length, 2);
  assert.throws(() => collectWorkerNotices({ ...options, compilerDirectories: [binary] }), /No matching esbuild license owner/);
  assert.throws(() => collectWorkerNotices({ ...options, compilerNotices: { ...compilerNotices, esbuildVersion: '2' } }), /Missing pinned Go runtime notices/);
  fs.rmSync(path.join(f.directory, 'node_modules/dep/LICENSE'));
  assert.throws(() => collectWorkerNotices(options), /No upstream license text for bundled npm dependency dep/);
  assert.throws(() => collectWorkerNotices({ ...options, bundledInputs: ['node_modules/missing/index.js'] }), /ENOENT/);
});

test('rendering rejects missing and modified text rather than shipping partial attribution', () => {
  const document = noticeDocument([{ name: 'dep', version: '1', notices: [{ source: 'LICENSE', text: 'fixture license' }] }], 'test boundary');
  const digest = document.packages[0].notices[0].sha256;
  assert.throws(() => renderNotices({ ...document, texts: {} }), /Missing or corrupt/);
  assert.throws(() => renderNotices({ ...document, texts: { [digest]: 'modified' } }), /Missing or corrupt/);
  assert.throws(() => renderNotices({ schemaVersion: 1, packages: [], texts: {} }), /Malformed/);
});

test('changed speech release bytes invalidate their pinned notice inventory', () => {
  assert.throws(() => collectSherpaNotices({ ...sherpaAssets, 'linux-x64': ['changed', 'changed'] }), /release assets changed/);
});

