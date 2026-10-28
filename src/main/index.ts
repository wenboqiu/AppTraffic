import { app, Tray, Menu, nativeImage, shell, clipboard } from 'electron';
import path from 'path';
import { existsSync } from 'fs';
import { createPopupWindow, togglePopup, isPopupVisible } from './popup';
import { registerIpcHandlers, pushSnapshot } from './ipc';
import { startCollector, stopCollector, onTick, getSnapshot } from './services/collector';
import { importLegacyStore, loadStore, storePath } from './services/store';
import {
  startAnalytics,
  stopAnalytics,
  isAnalyticsEnabled,
  setAnalyticsEnabled,
} from './services/analytics';
import { fmtBytes, fmtSpeed } from '../shared/format';
import { WECHAT_ACCOUNT } from '../shared/brand';

// Single-instance lock
if (!app.requestSingleInstanceLock()) {
  app.quit();
  process.exit(0);
}

let tray: Tray | null = null;
let trayMenu: Menu | null = null;

app.whenReady().then(() => {
  // Groups notifications / taskbar entries under the app (Windows only)
  if (process.platform === 'win32') app.setAppUserModelId('me.xueshi.apptraffic');
  // Menu-bar app: no Dock icon
  if (process.platform === 'darwin') app.dock?.hide();

  importLegacyStore();
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
  tray.setToolTip('AppTraffic');
  updateTrayMenu(tray);

  tray.on('click', () => togglePopup(tray!));
  tray.on('right-click', () => tray!.popUpContextMenu(trayMenu ?? undefined));

  onTick(() => {
    pushSnapshot();
    updateTooltip();
  });
  startCollector();
  startAnalytics();
});

app.on('second-instance', () => {
  if (tray && !isPopupVisible()) togglePopup(tray);
});

// Flush the in-memory counters before exiting
app.on('before-quit', () => {
  stopCollector();
  stopAnalytics();
});

// Having any listener stops Electron quitting when all windows close (tray-only app)
app.on('window-all-closed', () => {});

let lastTooltip = '';
function updateTooltip(): void {
  if (!tray) return;
  const s = getSnapshot('today');
  const text = s.sources.some((src) => src.state === 'on')
    ? `AppTraffic\n今日 ↓${fmtBytes(s.totalDown)} ↑${fmtBytes(s.totalUp)}\n实时 ↓${fmtSpeed(s.speedDown)} ↑${fmtSpeed(s.speedUp)}`
    : 'AppTraffic\n未在统计：系统流量未启用，也未检测到代理';
  if (text !== lastTooltip) {
    tray.setToolTip(text);
    lastTooltip = text;
  }
}

function updateTrayMenu(t: Tray): void {
  const menu = Menu.buildFromTemplate([
    {
      label: '打开 AppTraffic',
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
      label: '匿名使用统计',
      type: 'checkbox',
      checked: isAnalyticsEnabled(),
      click: (item) => {
        setAnalyticsEnabled(item.checked);
        updateTrayMenu(t);
      },
    },
    {
      label: '打开数据目录',
      click: () => shell.showItemInFolder(storePath()),
    },
    { type: 'separator' },
    {
      label: `关注公众号「${WECHAT_ACCOUNT}」（复制名称）`,
      click: () => clipboard.writeText(WECHAT_ACCOUNT),
    },
    { type: 'separator' },
    { label: '退出', role: 'quit' },
  ]);

  trayMenu = menu;
  // On macOS a context menu set on the tray takes over the left click, which opens the popup
  if (process.platform !== 'darwin') t.setContextMenu(menu);
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
