'use strict';

// Puts the engine beside the addon. The addon records a bare `libembedded_mongodb_native.so`
// and an rpath of its own directory, so the library has to sit next to it: here, for the tests
// and for a checkout used directly, and in the platform package that is published.

const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const ENGINE = 'libembedded_mongodb_native.so';
const root = path.join(__dirname, '..');

const addon = fs.readdirSync(root).find((name) => /^embedded-mongodb\..+\.node$/.test(name));
if (!addon) throw new Error('no addon found; run `napi build --platform` first');
const platform = addon.slice('embedded-mongodb.'.length, -'.node'.length);

// Newest first: a target directory keeps the output of every fingerprint it has ever had, and
// an older one would be a different library.
const buildDir = path.join(process.env.CARGO_TARGET_DIR ?? path.join(root, '..', 'target'), 'release', 'build');
const engine = fs
  .readdirSync(buildDir)
  .filter((name) => name.startsWith('embedded-mongodb-sys-'))
  .map((name) => path.join(buildDir, name, 'out', ENGINE))
  .filter((candidate) => fs.existsSync(candidate))
  .sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs)[0];
if (!engine) throw new Error(`no ${ENGINE} under ${buildDir}; run cargo build --release first`);

const packageDir = path.join(root, 'npm', platform);
for (const destination of [root, packageDir]) {
  fs.copyFileSync(engine, path.join(destination, ENGINE));
  fs.chmodSync(path.join(destination, ENGINE), 0o755);
}
fs.copyFileSync(path.join(root, addon), path.join(packageDir, addon));

if (process.platform === 'darwin') {
  // On macOS the build script gives the library an absolute install name so cargo's own tests
  // can find it, and the addon copied that name verbatim. Point it at the addon's directory.
  for (const copy of [path.join(root, addon), path.join(packageDir, addon)]) {
    const linked = execFileSync('otool', ['-L', copy], { encoding: 'utf8' })
      .split('\n')
      .map((line) => line.trim().split(' ')[0])
      .find((name) => name.endsWith(ENGINE));
    if (!linked) throw new Error(`${copy} does not link ${ENGINE}`);
    execFileSync('install_name_tool', ['-change', linked, `@loader_path/${ENGINE}`, copy]);
    execFileSync('codesign', ['--force', '--sign', '-', copy]);
  }
}

console.log(`bundled ${ENGINE} beside ${addon} and into ${path.relative(root, packageDir)}`);
