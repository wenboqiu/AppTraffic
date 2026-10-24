// electron-builder afterAllArtifactBuild hook: copies the installers of this build
// into dist/<version>/, so each version's folder holds its packages for every OS
// (intermediate files stay in dist/.build). Builds on different machines fill the
// same folder once their installers are copied over.
const fs = require('fs');
const path = require('path');

const INSTALLER = /\.(exe|dmg|zip|pkg|AppImage|deb|rpm)$/i;

exports.default = async function collectDist(result) {
  const { version } = require('../package.json');
  const target = path.join(__dirname, '..', 'dist', version);
  fs.mkdirSync(target, { recursive: true });
  for (const file of result.artifactPaths) {
    if (!INSTALLER.test(file)) continue;
    const dest = path.join(target, path.basename(file));
    fs.copyFileSync(file, dest);
    console.log(`  • copied to ${path.relative(path.join(__dirname, '..'), dest)}`);
  }
  // Nothing extra to publish
  return [];
};
