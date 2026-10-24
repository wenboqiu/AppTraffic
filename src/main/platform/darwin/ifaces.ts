/**
 * macOS network interfaces for nettop rows: stable indices, which ones are VPNs,
 * and the default-route interface (where unseen connections are assumed to go).
 */

import { execFile } from 'child_process';
import type { SysIface } from '../../services/sources/types';

const REFRESH_MS = 10_000;
/** Point-to-point tunnels VPN clients create (utun: Network Extension / WireGuard / Tailscale…) */
const TUNNEL_NAME = /^(utun|ppp|ipsec|tun|tap|wg)\d*$/;
/** mihomo / sing-box TUN default range (fake-ip) */
const CLASH_TUN_NET = /^198\.(18|19)\./;

function run(file: string, args: string[]): Promise<string> {
  return new Promise((resolve) => execFile(file, args, (err, stdout) => resolve(err ? '' : stdout)));
}

export interface IfaceInfo {
  name: string;
  addrs: string[];
}

/** `ifconfig` -> interfaces with their addresses. Exported for tests. */
export function parseIfconfig(out: string): IfaceInfo[] {
  const list: IfaceInfo[] = [];
  for (const line of out.split('\n')) {
    const head = line.match(/^([\w.]+): flags=/);
    if (head) {
      list.push({ name: head[1], addrs: [] });
      continue;
    }
    const addr = line.match(/^\s+inet6? ([0-9a-f.:]+)/i);
    if (addr && list.length) list[list.length - 1].addrs.push(addr[1]);
  }
  return list;
}

/**
 * A tunnel interface that carries traffic: it has a routable address. macOS keeps
 * several idle utuns (iCloud, Continuity…) with link-local IPv6 only.
 */
export function isVpnIface(f: IfaceInfo): boolean {
  return TUNNEL_NAME.test(f.name) && f.addrs.some((a) => !/^(fe80:|169\.254\.)/i.test(a));
}

/** Names of connected VPN services in `scutil --nc list` ("WireGuard", "Tailscale"…). Exported for tests. */
export function connectedVpnNames(out: string): string[] {
  return out
    .split('\n')
    .filter((l) => /\(Connected\)/.test(l))
    .map((l) => l.match(/"([^"]+)"/)?.[1])
    .filter((n): n is string => !!n);
}

export class InterfaceTable {
  private indices = new Map<string, number>();
  private info = new Map<string, IfaceInfo>();
  private vpnNames: string[] = [];
  private dflt = 'en0';
  private lastRefresh = 0;

  index(name: string): number {
    let i = this.indices.get(name);
    if (i === undefined) {
      i = this.indices.size + 1;
      this.indices.set(name, i);
    }
    return i;
  }

  defaultIface(): string {
    return this.dflt;
  }

  refreshIfStale(): void {
    if (Date.now() - this.lastRefresh > REFRESH_MS) this.refresh();
  }

  refresh(): void {
    this.lastRefresh = Date.now();
    void Promise.all([
      run('/sbin/ifconfig', []),
      run('/usr/sbin/scutil', ['--nc', 'list']),
      run('/sbin/route', ['-n', 'get', 'default']),
    ]).then(([ifconfig, nc, route]) => {
      if (ifconfig) this.info = new Map(parseIfconfig(ifconfig).map((f) => [f.name, f]));
      this.vpnNames = connectedVpnNames(nc);
      const m = route.match(/interface:\s*(\S+)/);
      if (m) this.dflt = m[1];
    });
  }

  /** Interfaces nettop rows referred to so far. */
  list(): SysIface[] {
    const vpns = [...this.indices.keys()].filter((n) => {
      const f = this.info.get(n);
      return f && isVpnIface(f) && !f.addrs.some((a) => CLASH_TUN_NET.test(a));
    });
    return [...this.indices].map(([name, index]) => {
      const f = this.info.get(name);
      const vpn = !!f && isVpnIface(f);
      const clashTun = !!f?.addrs.some((a) => CLASH_TUN_NET.test(a));
      // With a single tunnel up and a single VPN service connected, they're the same
      const alias = vpn && !clashTun && vpns.length === 1 && this.vpnNames.length === 1 ? this.vpnNames[0] : name;
      return { index, alias, desc: clashTun ? `${name} (Clash TUN)` : name, vpn };
    });
  }
}
