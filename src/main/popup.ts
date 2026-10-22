/**
 * Manages the borderless popup window that appears when the tray icon is clicked.
 * Positions itself above the tray icon and hides on focus loss.
 */

import { BrowserWindow, screen, Tray, shell } from 'electron';
import path from 'path';

const POPUP_WIDTH = 420;
const POPUP_HEIGHT = 600; // initial height; replaced once the renderer reports content height
const MIN_HEIGHT = 200;

let popup: BrowserWindow | null = null;
let currentHeight = POPUP_HEIGHT;
let visible = false;

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

  // Center horizontally over the tray icon, flush with taskbar top
  let x = Math.round(trayBounds.x + trayBounds.width / 2 - POPUP_WIDTH / 2);
  const y = workArea.y + workArea.height - currentHeight - 4;

  // Clamp so the window doesn't go off-screen
  x = Math.max(workArea.x, Math.min(x, workArea.x + workArea.width - POPUP_WIDTH));

  popup.setBounds({ x, y, width: POPUP_WIDTH, height: currentHeight });
}

/**
 * Resize the popup to fit its content height (reported by the renderer),
 * keeping it bottom-anchored above the tray. Clamped to the screen.
 */
export function setPopupHeight(height: number): void {
  if (!popup) return;

  const [x, y0] = popup.getPosition();
  const workArea = screen.getDisplayNearestPoint({ x, y: y0 }).workArea;
  const maxHeight = workArea.height - 8;
  const h = Math.max(MIN_HEIGHT, Math.min(Math.round(height), maxHeight));
  if (h === currentHeight) return;

  currentHeight = h;
  const y = workArea.y + workArea.height - h - 4;
  popup.setBounds({ x, y, width: POPUP_WIDTH, height: h });
}
