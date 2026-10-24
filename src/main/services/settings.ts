/**
 * User preferences persisted to userData/settings.json.
 */

import { app } from 'electron';
import path from 'path';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'fs';

interface Settings {
  /** The user turned on system-wide statistics; start the ETW helper on launch. */
  etwEnabled: boolean;
}

const DEFAULTS: Settings = { etwEnabled: false };
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
