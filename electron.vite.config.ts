import { readFileSync } from 'fs';
import { resolve } from 'path';
import { defineConfig, externalizeDepsPlugin } from 'electron-vite';
import react from '@vitejs/plugin-react';

const { version } = JSON.parse(readFileSync(resolve('package.json'), 'utf8')) as { version: string };

export default defineConfig({
  main: {
    plugins: [externalizeDepsPlugin({ exclude: [] })],
    resolve: {
      alias: { '@shared': resolve('src/shared') },
    },
  },
  preload: {
    plugins: [externalizeDepsPlugin()],
    resolve: {
      alias: { '@shared': resolve('src/shared') },
    },
  },
  renderer: {
    plugins: [react()],
    define: { __APP_VERSION__: JSON.stringify(version) },
    resolve: {
      alias: { '@shared': resolve('src/shared') },
    },
  },
});
