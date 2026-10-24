/**
 * Manages the borderless popup window that appears when the tray icon is clicked.
 * Positions itself next to the tray icon (above a bottom taskbar, below a top
 * taskbar or the macOS menu bar) and hides on focus loss.
 */

import { BrowserWindow, screen, Tray, shell } from 'electron';
import path from 'path';

const POPUP_WIDTH = 420;
const POPUP_HEIGHT = 600; // initial height; replaced once the renderer reports content height
const MIN_HEIGHT = 200;

let popup: BrowserWindow | null = null;
let currentHeight = POPUP_HEIGHT;
let visible = false;
/** Tray sits at the top of the screen: grow downwards from there */
let anchorTop = false;

export function createPopupWindow(preloadPath: string): BrowserWindow {
  popup = new BrowserWindow({
    width: POPUP_WIDTH,
    height: POPUP_HEIGHT,
    frame: false,
    transparent: true,
    resizable: false,
    movable: false,
    skipTaskbar: true,
    alwaysOnTop: true,
    show: false,
    backgroundColor: '#00000000',
    webPreferences: {
      preload: preloadPath,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
    },
  });

  // Hide when focus is lost (but not when DevTools opens)
  popup.on('blur', () => {
    if (visible && !popup?.webContents.isDevToolsOpened()) {
      softHide();
    }
  });

  // Open external links in the default browser
  popup.webContents.setWindowOpenHandler(({ url }) => {
    shell.openExternal(url);
    return { action: 'deny' };
  });

  // Render the window from the start (fully transparent + click-through) instead
  // of leaving it hidden. A hidden window has its rendering throttled, so the
  // renderer's ResizeObserver doesn't run until the window is shown — which made
  // the height land one frame *after* it appeared (the visible flash/jump).
  // Kept always-shown, it's pre-sized before every reveal; showing is just an
  // opacity flip.
  popup.setOpacity(0);
  popup.setIgnoreMouseEvents(true, { forward: true });
  popup.showInactive();

  return popup;
}

export function togglePopup(tray: Tray): void {
  if (!popup) return;

  if (visible) {
    softHide();
    return;
  }

  positionNearTray(tray);
  popup.setIgnoreMouseEvents(false);
  popup.setOpacity(1);
  popup.focus();
  visible = true;
}

export function hidePopup(): void {
  softHide();
}

/** Hide without un-rendering: keeps the renderer live so the next reveal is pre-sized. */
function softHide(): void {
  if (!popup) return;
  visible = false;
  popup.setOpacity(0);
  popup.setIgnoreMouseEvents(true, { forward: true });
}

export function getPopup(): BrowserWindow | null {
  return popup;
}

export function isPopupVisible(): boolean {
  return visible;
}

function positionNearTray(tray: Tray): void {
  if (!popup) return;

  const trayBounds = tray.getBounds();
  const display = screen.getDisplayNearestPoint({ x: trayBounds.x, y: trayBounds.y });
  const workArea = display.workArea;
  anchorTop = trayBounds.y < display.bounds.y + display.bounds.height / 2;

  // Center horizontally on the tray icon, flush with the taskbar / menu bar
  let x = Math.round(trayBounds.x + trayBounds.width / 2 - POPUP_WIDTH / 2);
  const y = verticalPosition(workArea, currentHeight);

  // Clamp so the window doesn't go off-screen
  x = Math.max(workArea.x, Math.min(x, workArea.x + workArea.width - POPUP_WIDTH));

  popup.setBounds({ x, y, width: POPUP_WIDTH, height: currentHeight });
}

function verticalPosition(workArea: Electron.Rectangle, height: number): number {
  return anchorTop ? workArea.y + 4 : workArea.y + workArea.height - height - 4;
}

/**
 * Resize the popup to fit its content height (reported by the renderer),
 * keeping it anchored at the tray's edge of the screen. Clamped to the screen.
 */
export function setPopupHeight(height: number): void {
  if (!popup) return;

  const [x, y0] = popup.getPosition();
  const workArea = screen.getDisplayNearestPoint({ x, y: y0 }).workArea;
  const maxHeight = workArea.height - 8;
  const h = Math.max(MIN_HEIGHT, Math.min(Math.round(height), maxHeight));
  if (h === currentHeight) return;

  currentHeight = h;
  const y = verticalPosition(workArea, h);
  popup.setBounds({ x, y, width: POPUP_WIDTH, height: h });
}
