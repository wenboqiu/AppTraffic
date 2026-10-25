import type { ElectronAPI } from '../../preload';

declare global {
  /** package.json version, injected by electron.vite.config.ts */
  const __APP_VERSION__: string;

  interface Window {
    electronAPI: ElectronAPI;
  }
}
