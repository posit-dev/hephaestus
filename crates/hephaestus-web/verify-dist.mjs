// Check the assembled package the way a consumer meets it: load the entry
// point, instantiate the wasm, and confirm the manifest describes what is
// actually on disk. Catches the failures that only appear after publishing —
// a renamed export, a file missing from `files`, a stale version.
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const dir = fileURLToPath(new URL('./dist/', import.meta.url));
const fail = (m) => { console.error('FAIL: ' + m); process.exitCode = 1; };
const ok = (m) => console.log('ok: ' + m);

const pkg = JSON.parse(readFileSync(dir + 'package.json', 'utf8'));

// 1. Every file the manifest promises exists.
for (const f of pkg.files) {
  if (existsSync(dir + f)) ok(`files[] present: ${f}`);
  else fail(`files[] lists ${f}, which is not in dist/`);
}

// 2. The entry point in `exports` is one of them.
const entry = pkg.exports['.'].default.replace('./', '');
if (pkg.files.includes(entry)) ok(`exports "." -> ${entry}, and it is published`);
else fail(`exports "." -> ${entry}, which is not in files[]`);

// 3. The npm version matches the crate version.
const crateVersion = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync('Cargo.toml', 'utf8'))[1];
if (pkg.version === crateVersion) ok(`version ${pkg.version} matches Cargo.toml`);
else fail(`package.json is ${pkg.version} but Cargo.toml is ${crateVersion}`);

// 4. The wrapper imports the glue by a package-relative path, not a sibling
//    directory — the mistake that only breaks once published.
const wrapper = readFileSync(dir + entry, 'utf8');
if (/from '\.\/hephaestus_web\.js'/.test(wrapper)) ok('wrapper imports ./hephaestus_web.js');
else fail('wrapper does not import the glue from ./ — it will not resolve when published');

// 5. It loads, instantiates, and exports what the types claim.
globalThis.fetch = () => Promise.reject(new Error('no network'));
const mod = await import(dir + entry);
await mod.default({ module_or_path: readFileSync(dir + 'hephaestus_web_bg.wasm') });
ok('wasm instantiates from the published bytes');

const expected = ['default', 'isSupported', 'documentFormatVersion', 'registerFont',
                  'setGenericFamily', 'registerFontFromUrl', 'registerGoogleFont', 'PlotView'];
const missing = expected.filter((k) => !(k in mod));
if (missing.length) fail(`entry point is missing exports: ${missing.join(', ')}`);
else ok(`entry point exports all ${expected.length} public names`);

// 6. The .d.ts declares the same names, so types cannot drift from runtime.
const dts = readFileSync(dir + 'hephaestus.d.ts', 'utf8');
const undeclared = expected.filter((k) =>
  k === 'default' ? !/export default function/.test(dts)
                  : !new RegExp(`export (declare )?(function|class) ${k}\\b`).test(dts));
if (undeclared.length) fail(`hephaestus.d.ts does not declare: ${undeclared.join(', ')}`);
else ok('hephaestus.d.ts declares every export');

// 7. The document format major is the coupling a consumer has to pin against.
const major = mod.documentFormatVersion();
if (Number.isInteger(major) && major > 0) ok(`document format major = ${major}`);
else fail(`documentFormatVersion() returned ${major}`);

const size = readFileSync(dir + 'hephaestus_web_bg.wasm').length;
console.log(`\nwasm: ${size} bytes raw`);
