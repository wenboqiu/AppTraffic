import { ipcMain, shell } from 'electron';
import { getSnapshot, runSourceAction } from './services/collector';
import { storePath } from './services/store';
import { getPopup, hidePopup, isPopupVisible, setPopupHeight } from './popup';
import type { Range } from '../shared/types';

let currentRange: Range = 'today';

export function registerIpcHandlers(): void {
  ipcMain.handle('get-snapshot', (_e, range: Range) => {
    currentRange = range;
    return getSnapshot(range);
  });

  ipcMain.handle('source-action', (_e, id: string) => runSourceAction(id));

  ipcMain.on('show-in-folder', (_e, filePath: string) => {
    if (filePath) shell.showItemInFolder(filePath);
  });

  ipcMain.on('open-data-folder', () => shell.showItemInFolder(storePath()));

  ipcMain.on('close-popup', () => hidePopup());

  ipcMain.on('resize-popup', (_e, height: number) => setPopupHeight(height));
}

/** Push a fresh snapshot to the popup — only while it's visible, so idle cost stays ~0. */
export function pushSnapshot(): void {
  const popup = getPopup();
  if (!popup || popup.isDestroyed() || !isPopupVisible()) return;
  popup.webContents.send('snapshot', getSnapshot(currentRange));
}
