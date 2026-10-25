/**
 * Anonymous usage statistics, reported to a self-hosted Umami instance.
 *
 * Only a random install ID, the app version and the OS are sent — never any
 * traffic numbers, hosts or app names. Failures are dropped silently. Turned
 * off from the tray menu; development builds don't report unless
 * APPTRAFFIC_ANALYTICS=1.
 */

import { app, screen } from 'electron';
import { randomUUID } from 'crypto';
import { getSettings, updateSettings } from './settings';

const ENDPOINT = 'https://umami.wondergarden.top/api/send';
const WEBSITE_ID = 'd2ec23fe-003a-4689-9ef0-6f2cd5a0d3d9';
const HOSTNAME = 'apptraffic.app';
/** How often to check whether the day rolled over (the app runs for days). */
const ACTIVE_CHECK_MS = 60 * 60 * 1000;

/**
 * Umami derives the visitor from ip + user agent + install ID, so a changing
 * IP (hotspot, new network) or an app upgrade would count as a new visitor.
 * Pin both: the visitor then only depends on the install ID (and Umami's
 * monthly salt). Costs the geo breakdown, which needs the real IP.
 */
const FIXED_IP = '127.0.0.1';
const CLIENT =
  process.platform === 'darwin'
    ? { userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppTraffic', os: 'Mac OS' }
    : { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppTraffic', os: 'Windows 10' };

let timer: NodeJS.Timeout | null = null;

function enabled(): boolean {
  if (!getSettings().analyticsEnabled) return false;
  return app.isPackaged || process.env['APPTRAFFIC_ANALYTICS'] === '1';
}

function installId(): string {
  let id = getSettings().installId;
  if (!id) {
    id = randomUUID();
    updateSettings({ installId: id });
  }
  return id;
}

/** Local date, YYYY-MM-DD */
function today(): string {
  const d = new Date();
  const pad = (n: number): string => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

async function send(name: string): Promise<void> {
  if (!enabled()) return;
  const { width, height } = screen.getPrimaryDisplay().size;
  const body = {
    type: 'event',
    payload: {
      website: WEBSITE_ID,
      hostname: HOSTNAME,
      url: '/app',
      title: 'AppTraffic',
      language: app.getLocale(),
      screen: `${width}x${height}`,
      name,
      id: installId(),
      ip: FIXED_IP,
      userAgent: CLIENT.userAgent,
      os: CLIENT.os,
      browser: 'AppTraffic',
      device: 'desktop',
      data: { version: app.getVersion(), os: process.platform },
    },
  };
  try {
    await fetch(ENDPOINT, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        // Umami drops requests without a browser-like UA
        'User-Agent': CLIENT.userAgent,
      },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(10_000),
    });
  } catch {
    // offline / server down: drop it
  }
}

/** Report once per local day, however long the app has been running. */
function checkActive(): void {
  const day = today();
  if (getSettings().lastActiveDay === day || !enabled()) return;
  updateSettings({ lastActiveDay: day });
  void send('app_active');
}

export function startAnalytics(): void {
  if (enabled()) {
    if (!getSettings().installReported) {
      updateSettings({ installReported: true });
      void send('app_install');
    }
    void send('app_start');
    checkActive();
  }
  timer ??= setInterval(checkActive, ACTIVE_CHECK_MS);
}

export function stopAnalytics(): void {
  if (timer) clearInterval(timer);
  timer = null;
}

export function isAnalyticsEnabled(): boolean {
  return getSettings().analyticsEnabled;
}

export function setAnalyticsEnabled(on: boolean): void {
  updateSettings({ analyticsEnabled: on });
  if (on) checkActive();
}
