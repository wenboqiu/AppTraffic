// 打包后调用：把本机生成的安装包按版本号收集到 dist/<版本号>/
// （中间文件仍在 src-tauri/target/release/bundle）。Windows 和 Mac 分别在各自机器上打包，
// Mac 的 dmg 拷到同一目录即可。
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(fileURLToPath(import.meta.url), '..', '..');
const { version } = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
const targetDir = join(root, 'src-tauri', 'target');
// 默认构建在 release/bundle；Mac 用 --target 构建时在 <triple>/release/bundle
const triples = existsSync(targetDir) ? readdirSync(targetDir).filter((d) => d.endsWith('-apple-darwin')) : [];
const bundles = [join(targetDir, 'release', 'bundle'), ...triples.map((t) => join(targetDir, t, 'release', 'bundle'))];
const ARCH = { aarch64: 'arm64' };
const out = join(root, 'dist', version);
mkdirSync(out, { recursive: true });

// 产物名：AppTraffic-<版本>-<平台>-<架构>[-setup].<后缀>
const rules = [
  { dir: 'nsis', ext: '.exe', name: (a) => `AppTraffic-${version}-win-${a}-setup.exe` },
  { dir: 'dmg', ext: '.dmg', name: (a) => `AppTraffic-${version}-mac-${a}.dmg` },
];

let n = 0;
for (const r of rules) {
  for (const dir of bundles.map((b) => join(b, r.dir)).filter(existsSync)) {
    for (const f of readdirSync(dir)) {
      const head = `AppTraffic_${version}_`;
      if (!f.startsWith(head) || !f.endsWith(r.ext)) continue;
      const arch = f.slice(head.length, -r.ext.length).replace(/-setup$/, '');
      const dest = join(out, r.name(ARCH[arch] ?? arch));
      copyFileSync(join(dir, f), dest);
      console.log(`  • copied to ${relative(root, dest)}`);
      n++;
    }
  }
}
if (!n) {
  console.error(`没有找到 ${version} 的安装包，先跑 tauri build`);
  process.exit(1);
}
