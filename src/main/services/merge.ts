/**
 * Reconciles the Clash connection list (L2) with system-wide ETW counts (L0) so
 * every byte is booked once, under the app that caused it.
 *
 * ETW doesn't count loopback as traffic, but reports it as client -> local server
 * flows. That gives two exact volume measures per app:
 * - external: the app's own sockets to the outside (direct traffic, and TUN-captured
 *   traffic, since the TUN adapter is just another interface to the app)
 * - via core: what the app exchanged with the proxy core's local ports (system proxy,
 *   SOCKS, explicit -x …), including connections too short for Clash's 1 s polling
 *
 * With Clash reachable, the core's own sockets are dropped (they're the upstream
 * side of the above) and Clash's per-connection numbers are used only to tell
 * proxied from direct and to name hosts; its "gap" bucket is redundant then.
 * Clash bytes ETW never shows (e.g. it can't see the TUN adapter) are booked after
 * a grace period.
 *
 * L1: for a local proxy without a Clash API, the proxy process's upstream bytes are
 * split among its loopback clients in proportion to what each one exchanged.
 *
 * VPNs: ETW also says which interface each byte used. Bytes on a VPN adapter are
 * booked to that VPN; the VPN client's own traffic (the encrypted carrier of the
 * same bytes) is booked to the TUNNEL route, which totals leave out.
 */

import type { Delta } from './store';
import type { ClashDelta, ClashSample, SysLoop, SystemSample } from './sources/types';
import { isCore, isVpnClient, procName, stem } from './cores';
import { CORE_INNER, TUNNEL, VPN_ROUTE_PREFIX } from '../../shared/types';

/** Adapter names a Clash-compatible core gives its TUN device. */
const CLASH_TUN_HINT = /mihomo|clash|sing-?box|\bmeta\b/i;

/**
 * VPN adapters by interface index -> alias. A Clash core's own TUN adapter is left
 * out while its API is reachable: Clash's connection list says what went through it.
 */
function vpnAdapters(etw: SystemSample, clash: ClashSample | null): Map<number, string> {
  const out = new Map<number, string>();
  const tun = clash?.tunDevice?.toLowerCase();
  for (const f of etw.ifaces) {
    if (!f.vpn) continue;
    if (clash && (f.alias.toLowerCase() === tun || CLASH_TUN_HINT.test(`${f.alias} ${f.desc}`))) continue;
    out.set(f.index, f.alias);
  }
  return out;
}

/** Ticks without ETW bytes on a ledger before Clash's unmatched bytes are booked. */
const GRACE_TICKS = 5;
/** Forget a process's ledgers after this long without activity. */
const IDLE_FORGET_TICKS = 120;

interface Volume {
  up: number;
  down: number;
}

interface Settled {
  up: number;
  down: number;
  proxied: number;
  /** Clash hosts, set only when Clash bytes were booked in ETW's absence. */
  fallbackHosts: [string, number][] | null;
}

/**
 * Running totals for one process on one path (external or via core): ETW gives
 * the volume as soon as it arrives; Clash gives the proxied share, possibly late.
 */
class Ledger {
  private etwUp = 0;
  private etwDown = 0;
  private clashUp = 0;
  private clashDown = 0;
  private clashProxied = 0;
  private bookedUp = 0;
  private bookedDown = 0;
  private bookedProxied = 0;
  private sinceEtw = 0;
  private clashHosts = new Map<string, number>();

  /**
   * @param proxiedByRatio Everything on this path went through Clash, so bytes Clash
   *   missed are assumed to split like the ones it saw. Otherwise (external path,
   *   which also carries non-Clash traffic) only bytes Clash reported count as proxied.
   */
  constructor(private proxiedByRatio: boolean) {}

  step(etw: Volume | undefined, clash: ClashDelta[]): Settled {
    if (etw && etw.up + etw.down > 0) {
      this.etwUp += etw.up;
      this.etwDown += etw.down;
      this.sinceEtw = 0;
      this.clashHosts.clear();
    } else {
      this.sinceEtw++;
    }
    for (const c of clash) {
      this.clashUp += c.up;
      this.clashDown += c.down;
      this.clashProxied += c.up + c.down - c.direct;
      for (const [h, n] of c.hosts) this.clashHosts.set(h, (this.clashHosts.get(h) ?? 0) + n);
    }

    let targetUp = this.etwUp;
    let targetDown = this.etwDown;
    const fallback = this.sinceEtw >= GRACE_TICKS;
    if (fallback) {
      targetUp = Math.max(targetUp, this.clashUp);
      targetDown = Math.max(targetDown, this.clashDown);
    }
    const up = Math.max(0, targetUp - this.bookedUp);
    const down = Math.max(0, targetDown - this.bookedDown);
    this.bookedUp += up;
    this.bookedDown += down;
    let fallbackHosts: [string, number][] | null = null;
    if (fallback && up + down > 0) {
      fallbackHosts = [...this.clashHosts];
      this.clashHosts.clear();
    }

    // Proxied bytes can be reported after ETW already booked them as direct;
    // the next delta then carries a negative direct to move them across.
    const booked = this.bookedUp + this.bookedDown;
    const clashTotal = this.clashUp + this.clashDown;
    const target = this.proxiedByRatio
      ? clashTotal > 0
        ? Math.round((booked * this.clashProxied) / clashTotal)
        : 0
      : Math.min(this.clashProxied, booked);
    const proxied = target - this.bookedProxied;
    this.bookedProxied = target;
    return { up, down, proxied, fallbackHosts };
  }
}

interface ProcState {
  path: string;
  external: Ledger;
  viaCore: Ledger;
  idle: number;
}

function strip(d: ClashDelta): Delta {
  const { via: _via, known: _known, ...rest } = d;
  return rest;
}

export class Reconciler {
  private procs = new Map<string, ProcState>();
  /** Client name of the last Clash API seen, for labelling its core while the API is unreachable. */
  private lastRoute = '';

  reset(): void {
    this.procs.clear();
  }

  merge(clash: ClashSample | null, etw: SystemSample | null, proxyPorts: Set<number>): Delta[] {
    if (!etw) {
      this.procs.clear();
      return clash ? clash.deltas.map(strip) : [];
    }

    const out: Delta[] = [];
    const vpnIfaces = vpnAdapters(etw, clash);
    let vpnActive = false;

    // ETW external bytes per process name. Bytes on a VPN adapter are booked to that
    // VPN right away; only the rest (physical NICs, Clash's own TUN) goes on.
    const external = new Map<string, Delta>();
    for (const p of etw.procs) {
      const name = procName(p.path);
      let up = p.up;
      let down = p.down;
      let viaVpn: Delta | null = null;
      for (const [index, ifUp, ifDown] of p.ifs ?? []) {
        const alias = vpnIfaces.get(index);
        if (alias === undefined || ifUp + ifDown === 0) continue;
        vpnActive = true;
        up -= ifUp;
        down -= ifDown;
        const route = VPN_ROUTE_PREFIX + alias;
        viaVpn = { proc: name, path: p.path, up: ifUp, down: ifDown, direct: 0, route, newConns: 0, hosts: [] };
        out.push(viaVpn);
      }
      if (up + down <= 0) {
        if (viaVpn) viaVpn.hosts = p.hosts;
        continue;
      }
      const d = external.get(name) ?? { proc: name, path: p.path, up: 0, down: 0, direct: 0, route: '', newConns: 0, hosts: [] };
      d.up += Math.max(0, up);
      d.down += Math.max(0, down);
      d.hosts.push(...p.hosts);
      external.set(name, d);
    }
    // A VPN client's own traffic carries the bytes above a second time, encrypted
    for (const [name, d] of external) {
      if (!isVpnClient(name)) continue;
      out.push({ ...d, direct: 0, route: TUNNEL });
      external.delete(name);
    }

    if (!clash) {
      this.procs.clear();
      out.push(...this.attributeLocalProxies(external, etw.loops, proxyPorts, false));
      for (const d of external.values()) {
        // What's left on a core is still the core's own, not an app's: the carrier
        // of a TUN adapter's traffic, or its DNS and updates
        if (isCore(d.proc)) {
          const route = vpnActive ? TUNNEL : '';
          out.push({ ...d, proc: CORE_INNER, path: '', direct: route ? 0 : d.up + d.down, route });
        } else {
          out.push({ ...d, direct: d.up + d.down });
        }
      }
      return out;
    }

    this.lastRoute = clash.route;
    for (const name of [...external.keys()]) if (isCore(name)) external.delete(name);
    out.push(...this.attributeLocalProxies(external, etw.loops, proxyPorts, true));

    // What each app exchanged with the core's local ports
    const viaCore = new Map<string, Volume & { path: string }>();
    for (const l of etw.loops) {
      if (!isCore(procName(l.server))) continue;
      const name = procName(l.client);
      if (isCore(name)) continue;
      const v = viaCore.get(name) ?? { up: 0, down: 0, path: l.client };
      v.up += l.up;
      v.down += l.down;
      viaCore.set(name, v);
    }

    const tunByProc = new Map<string, ClashDelta[]>();
    const proxyByProc = new Map<string, ClashDelta[]>();
    for (const d of clash.deltas) {
      if (d.via === 'inner') out.push(strip(d));
      // The gap is short connections Clash's polling missed; ETW measured them
      if (d.via === 'inner' || d.via === 'gap') continue;
      if (!d.known) {
        // Unidentified Tun traffic is in ETW under its real process already; an
        // unidentified proxy-port client is usually a LAN device (allow-lan)
        if (d.via === 'proxy') out.push(strip(d));
        continue;
      }
      const map = d.via === 'tun' ? tunByProc : proxyByProc;
      const list = map.get(d.proc) ?? [];
      list.push(d);
      map.set(d.proc, list);
    }

    const names = new Set([
      ...external.keys(),
      ...viaCore.keys(),
      ...tunByProc.keys(),
      ...proxyByProc.keys(),
      ...this.procs.keys(),
    ]);
    for (const name of names) {
      const ext = external.get(name);
      const core = viaCore.get(name);
      const tun = tunByProc.get(name) ?? [];
      const prx = proxyByProc.get(name) ?? [];
      let st = this.procs.get(name);
      if (!st) {
        const path = ext?.path || core?.path || tun[0]?.path || prx[0]?.path || '';
        st = { path, external: new Ledger(false), viaCore: new Ledger(true), idle: 0 };
        this.procs.set(name, st);
      }
      const active = ext || core || tun.length || prx.length;
      st.idle = active ? 0 : st.idle + 1;

      const a = st.external.step(ext, tun);
      const b = st.viaCore.step(core, prx);
      const up = a.up + b.up;
      const down = a.down + b.down;
      const proxied = a.proxied + b.proxied;
      const newConns = [...tun, ...prx].reduce((n, d) => n + d.newConns, 0);
      // ETW knows external hosts (DNS); for proxied connections only Clash does
      const hosts: [string, number][] = [
        ...(ext?.hosts ?? []),
        ...prx.flatMap((d) => d.hosts),
        ...(a.fallbackHosts ?? []),
      ];

      if (st.idle >= IDLE_FORGET_TICKS) this.procs.delete(name);
      if (up + down === 0 && proxied === 0 && newConns === 0) continue;
      out.push({ proc: name, path: st.path, up, down, direct: up + down - proxied, route: clash.route, newConns, hosts });
    }
    return out;
  }

  /**
   * L1: split each local proxy's upstream bytes among the apps that sent it
   * loopback traffic this tick. Skipped for a Clash core when Clash is reachable,
   * since its connection list is exact.
   */
  private attributeLocalProxies(
    external: Map<string, Delta>,
    loops: SysLoop[],
    proxyPorts: Set<number>,
    clashUp: boolean,
  ): Delta[] {
    const byServer = new Map<string, SysLoop[]>();
    for (const l of loops) {
      if (!proxyPorts.has(l.port)) continue;
      const server = procName(l.server);
      if (clashUp && isCore(server)) continue;
      if (procName(l.client) === server) continue;
      const list = byServer.get(server) ?? [];
      list.push(l);
      byServer.set(server, list);
    }

    const out: Delta[] = [];
    for (const [server, list] of byServer) {
      const ext = external.get(server);
      if (!ext) continue;
      const sumUp = list.reduce((a, l) => a + l.up, 0);
      const sumDown = list.reduce((a, l) => a + l.down, 0);
      let givenUp = 0;
      let givenDown = 0;
      for (const l of list) {
        const up = sumUp ? Math.round((ext.up * l.up) / sumUp) : 0;
        const down = sumDown ? Math.round((ext.down * l.down) / sumDown) : 0;
        if (up + down === 0) continue;
        givenUp += up;
        givenDown += down;
        out.push({
          proc: procName(l.client),
          path: l.client,
          up,
          down,
          direct: 0,
          route: (isCore(server) && this.lastRoute) || stem(server),
          newConns: 0,
          hosts: [[`经 ${server} 代理`, up + down]],
        });
      }
      const before = ext.up + ext.down;
      ext.up = Math.max(0, ext.up - givenUp);
      ext.down = Math.max(0, ext.down - givenDown);
      const ratio = before ? (ext.up + ext.down) / before : 0;
      ext.hosts = ext.hosts.map(([h, n]) => [h, Math.round(n * ratio)]);
      if (ext.up + ext.down === 0) external.delete(server);
    }
    return out;
  }
}
