import { app, Tray, Menu, nativeImage, shell } from 'electron';
import path from 'path';
import { existsSync } from 'fs';
import { createPopupWindow, togglePopup, isPopupVisible } from './popup';
import { registerIpcHandlers, pushSnapshot } from './ipc';
import { startCollector, stopCollector, onTick, getSnapshot } from './services/collector';
import { loadStore, storePath } from './services/store';
import { fmtBytes, fmtSpeed } from '../shared/format';

// Single-instance lock
if (!app.requestSingleInstanceLock()) {
  app.quit();
  process.exit(0);
}

let tray: Tray | null = null;

app.whenReady().then(() => {
  app.setAppUserModelId('me.xueshi.clashtraffic');

  loadStore();
  registerIpcHandlers();

  const preload = path.join(__dirname, '../preload/index.js');
  const popup = createPopupWindow(preload);

  if (!app.isPackaged && process.env['ELECTRON_RENDERER_URL']) {
    popup.loadURL(process.env['ELECTRON_RENDERER_URL']);
  } else {
    popup.loadFile(path.join(__dirname, '../renderer/index.html'));
  }

  tray = new Tray(loadTrayIcon());
  tray.setToolTip('ClashTraffic');
  updateTrayMenu(tray);

  tray.on('click', () => togglePopup(tray!));
  tray.on('right-click', () => tray!.popUpContextMenu());

  onTick(() => {
    pushSnapshot();
    updateTooltip();
  });
  startCollector();
});

app.on('second-instance', () => {
  if (tray && !isPopupVisible()) togglePopup(tray);
});

// Flush the in-memory counters before exiting
app.on('before-quit', () => stopCollector());

// Having any listener stops Electron quitting when all windows close (tray-only app)
app.on('window-all-closed', () => {});

let lastTooltip = '';
function updateTooltip(): void {
  if (!tray) return;
  const s = getSnapshot('today');
  const text = s.sources.some((src) => src.state === 'on')
    ? `ClashTraffic\n今日 ↓${fmtBytes(s.totalDown)} ↑${fmtBytes(s.totalUp)}\n实时 ↓${fmtSpeed(s.speedDown)} ↑${fmtSpeed(s.speedUp)}`
    : 'ClashTraffic\n未在统计：系统流量未启用，也未检测到代理';
  if (text !== lastTooltip) {
    tray.setToolTip(text);
    lastTooltip = text;
  }
}

function updateTrayMenu(t: Tray): void {
  const menu = Menu.buildFromTemplate([
    {
      label: '打开 ClashTraffic',
      click: () => {
        if (!isPopupVisible()) togglePopup(t);
      },
    },
    {
      label: '开机自启',
      type: 'checkbox',
      checked: app.getLoginItemSettings().openAtLogin,
      click: (item) => {
        app.setLoginItemSettings({ openAtLogin: item.checked });
        updateTrayMenu(t);
      },
    },
    {
      label: '打开数据目录',
      click: () => shell.showItemInFolder(storePath()),
    },
    { type: 'separator' },
    { label: '退出', role: 'quit' },
  ]);

  t.setContextMenu(menu);
}

function loadTrayIcon(): Electron.NativeImage {
  const candidates = app.isPackaged
    ? [path.join(process.resourcesPath, 'resources', 'tray-icon.png')]
    : [
        path.join(app.getAppPath(), 'resources', 'tray-icon.png'),
        path.join(__dirname, '../../resources', 'tray-icon.png'),
      ];

  for (const p of candidates) {
    if (existsSync(p)) return nativeImage.createFromPath(p).resize({ width: 16, height: 16 });
  }
  return nativeImage.createEmpty();
}
