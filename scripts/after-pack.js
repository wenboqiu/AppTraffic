// electron-builder afterPack hook (macOS): electronLanguages only trims the app's own
// Resources, so drop the Electron framework's other *.lproj too, then re-sign
// ad hoc — editing the bundle breaks Electron's signature, and Apple silicon
// won't run unsigned code.
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const KEEP = new Set(['en.lproj', 'zh_CN.lproj', 'Base.lproj']);

exports.default = async function afterPack(context) {
  if (context.electronPlatformName !== 'darwin') return;
  const app = path.join(context.appOutDir, `${context.packager.appInfo.productFilename}.app`);
  const res = path.join(app, 'Contents', 'Frameworks', 'Electron Framework.framework', 'Versions', 'A', 'Resources');
  for (const name of fs.readdirSync(res)) {
    if (name.endsWith('.lproj') && !KEEP.has(name)) fs.rmSync(path.join(res, name), { recursive: true });
  }
  execFileSync('codesign', ['--force', '--deep', '--sign', '-', app], { stdio: 'inherit' });
};
