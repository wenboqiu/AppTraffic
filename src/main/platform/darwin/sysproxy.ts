/**
 * Reads the macOS system proxy (`scutil --proxy`) so loopback flows into that port
 * can be recognised as "app -> local proxy" hops.
 */

import { execFile } from 'child_process';

const REFRESH_MS = 15_000;
const LOOPBACK = new Set(['127.0.0.1', 'localhost', '::1']);

let ports = new Set<number>();
let lastRefresh = 0;

/** Exported for tests. */
export function parseScutilProxy(out: string): Set<number> {
  const value = (key: string) => out.match(new RegExp(`^\\s*${key}\\s*:\\s*(.*?)\\s*$`, 'm'))?.[1];
  const next = new Set<number>();
  for (const scheme of ['HTTP', 'HTTPS', 'SOCKS']) {
    if (value(`${scheme}Enable`) !== '1') continue;
    const host = (value(`${scheme}Proxy`) ?? '').toLowerCase();
    const port = Number(value(`${scheme}Port`));
    if (LOOPBACK.has(host) && port) next.add(port);
  }
  return next;
}

function refresh(): void {
  lastRefresh = Date.now();
  execFile('/usr/sbin/scutil', ['--proxy'], (err, stdout) => {
    if (!err) ports = parseScutilProxy(stdout);
  });
}

/** Loopback ports the system proxy currently points at (refreshed in the background). */
export function systemProxyPorts(): Set<number> {
  if (Date.now() - lastRefresh > REFRESH_MS) refresh();
  return ports;
}
