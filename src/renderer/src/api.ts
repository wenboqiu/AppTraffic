import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Range, Snapshot } from '../../shared/types';

/** 与 Rust 端 commands / 事件的桥：弹窗只通过这里和后端说话。 */
export const api = {
  getSnapshot: (range: Range): Promise<Snapshot> => invoke('get_snapshot', { range }),

  onSnapshot: (cb: (s: Snapshot) => void): (() => void) => {
    const un = listen<Snapshot>('snapshot', (e) => cb(e.payload));
    return () => void un.then((f) => f());
  },

  runSourceAction: (id: string): Promise<void> => invoke('source_action', { id }),

  showInFolder: (path: string): void => void invoke('show_in_folder', { path }),

  openDataFolder: (): void => void invoke('open_data_folder'),

  closePopup: (): void => void invoke('close_popup'),

  resizePopup: (height: number): void => void invoke('resize_popup', { height }),

  copyText: (text: string): void => {
    navigator.clipboard?.writeText(text).catch(() => {});
  },
};
