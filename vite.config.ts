import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const { version } = JSON.parse(readFileSync(resolve('package.json'), 'utf8')) as { version: string };

export default defineConfig({
  root: 'src/renderer',
  plugins: [react()],
  clearScreen: false,
  define: { __APP_VERSION__: JSON.stringify(version) },
  resolve: { alias: { '@shared': resolve('src/shared') } },
  server: { port: 1431, strictPort: true },
  build: { outDir: resolve('dist-web'), emptyOutDir: true, target: ['es2021', 'chrome105', 'safari15'] },
});
