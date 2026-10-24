/**
 * Reads the WinINet system proxy so ETW loopback flows into that port can be
 * recognised as "app -> local proxy" hops.
 */

import { execFile } from 'child_process';

const KEY = 'HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings';
const REFRESH_MS = 15_000;
const LOOPBACK = new Set(['127.0.0.1', 'localhost', '::1', '[::1]']);

let ports = new Set<number>();
let lastRefresh = 0;

function refresh(): void {
  lastRefresh = Date.now();
  execFile('reg.exe', ['query', KEY], { windowsHide: true }, (err, stdout) => {
    if (err) return;
    const value = (name: string) => stdout.match(new RegExp(`^\\s*${name}\\s+REG_\\w+\\s+(.*)$`, 'mi'))?.[1].trim();
    const next = new Set<number>();
    if (Number(value('ProxyEnable')) === 1) {
      // "127.0.0.1:7897" or per-scheme "http=127.0.0.1:7897;https=…"
      for (const entry of (value('ProxyServer') ?? '').split(';')) {
        const hostPort = entry.includes('=') ? entry.split('=')[1] : entry;
        const m = hostPort.trim().match(/^(.*):(\d+)$/);
        if (m && LOOPBACK.has(m[1].toLowerCase())) next.add(Number(m[2]));
      }
    }
    ports = next;
  });
}

/** Loopback ports the system proxy currently points at (refreshed in the background). */
export function systemProxyPorts(): Set<number> {
  if (Date.now() - lastRefresh > REFRESH_MS) refresh();
  return ports;
}
