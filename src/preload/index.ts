import { contextBridge, ipcRenderer } from 'electron';
import type { Range, Snapshot } from '../shared/types';

const api = {
  getSnapshot: (range: Range): Promise<Snapshot> =>
    ipcRenderer.invoke('get-snapshot', range),

  onSnapshot: (cb: (s: Snapshot) => void): (() => void) => {
    const handler = (_event: Electron.IpcRendererEvent, s: Snapshot) => cb(s);
    ipcRenderer.on('snapshot', handler);
    return () => ipcRenderer.removeListener('snapshot', handler);
  },

  runSourceAction: (id: string): Promise<void> => ipcRenderer.invoke('source-action', id),

  showInFolder: (filePath: string): void => {
    ipcRenderer.send('show-in-folder', filePath);
  },

  openDataFolder: (): void => {
    ipcRenderer.send('open-data-folder');
  },

  closePopup: (): void => {
    ipcRenderer.send('close-popup');
  },

  resizePopup: (height: number): void => {
    ipcRenderer.send('resize-popup', height);
  },
};

export type ElectronAPI = typeof api;

contextBridge.exposeInMainWorld('electronAPI', api);
