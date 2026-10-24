/**
 * Proxy cores that expose (or may expose) a Clash-compatible REST API, and the
 * GUI clients that launch them.
 */

import { UNKNOWN_PROCESS } from '../../shared/types';

/** Executable names of cores whose own sockets are the upstream side of app traffic. */
export const CORE_NAMES = [
  'verge-mihomo.exe',
  'verge-mihomo-alpha.exe',
  'mihomo.exe',
  'mihomo-alpha.exe',
  'mihomo-windows-amd64.exe',
  'clash.exe',
  'clash-meta.exe',
  'clash-meta-alpha.exe',
  'FlClashCore.exe',
  'sing-box.exe',
];
const CORE_SET = new Set(CORE_NAMES.map((n) => n.toLowerCase()));

/** Friendly names for the parent process that launched a core. */
const CLIENT_LABELS: Record<string, string> = {
  'clash-verge': 'Clash Verge',
  'clash-verge-service': 'Clash Verge',
  'mihomo-party': 'Mihomo Party',
  sparkle: 'Sparkle',
  flclash: 'FlClash',
  flclashhelperservice: 'FlClash',
  v2rayn: 'v2rayN',
  nekoray: 'NekoRay',
  nekobox: 'NekoBox',
  hiddify: 'Hiddify',
  'clash-for-windows': 'Clash for Windows',
  'gui.for.clash': 'GUI.for.Clash',
  'gui.for.singbox': 'GUI.for.SingBox',
};

export function procName(exePath: string): string {
  if (!exePath) return UNKNOWN_PROCESS;
  return exePath.split(/[\\/]/).pop() || exePath;
}

export function isCore(name: string): boolean {
  return CORE_SET.has(name.toLowerCase());
}

/** "Clash Verge" for a core started by clash-verge.exe; the core's own stem otherwise. */
export function clientLabel(parentName: string | null, coreName: string): string {
  const stem = (n: string) => n.replace(/\.exe$/i, '');
  if (parentName) {
    const label = CLIENT_LABELS[stem(parentName).toLowerCase().replace(/[\s_]+/g, '-')];
    if (label) return label;
  }
  return stem(coreName);
}
