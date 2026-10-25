/**
 * User preferences persisted to userData/settings.json.
 */

import { app } from 'electron';
import path from 'path';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'fs';

interface Settings {
  /** The user turned on system-wide statistics; start the ETW helper on launch. */
  etwEnabled: boolean;
  /** macOS: nettop needs no permission, so system statistics are on unless turned off. */
  nettopEnabled: boolean;
  /** Anonymous usage statistics (see analytics.ts); the user can turn it off from the tray menu. */
  analyticsEnabled: boolean;
  /** Random ID identifying this install in the statistics. */
  installId?: string;
  installReported?: boolean;
  /** Local date (YYYY-MM-DD) of the last daily-active report. */
  lastActiveDay?: string;
}

const DEFAULTS: Settings = { etwEnabled: false, nettopEnabled: true, analyticsEnabled: true };
let cache: Settings | null = null;

function file(): string {
  return path.join(app.getPath('userData'), 'settings.json');
}

export function getSettings(): Settings {
  if (cache) return cache;
  cache = { ...DEFAULTS };
  if (existsSync(file())) {
    try {
      cache = { ...DEFAULTS, ...JSON.parse(readFileSync(file(), 'utf8')) };
    } catch {
      // unreadable: fall back to defaults
    }
  }
  return cache!;
}

export function updateSettings(patch: Partial<Settings>): void {
  cache = { ...getSettings(), ...patch };
  mkdirSync(path.dirname(file()), { recursive: true });
  writeFileSync(file(), JSON.stringify(cache, null, 2));
}
