/**
 * macOS system-wide source: `nettop` in delta logging mode, which needs no root.
 * Every second it prints each process's bytes (including connections that already
 * closed) followed by its open connections with their interface:
 *
 *   ,interface,state,bytes_in,bytes_out,
 *   curl.19127,,,4463,283,
 *   tcp4 127.0.0.1:57597<->127.0.0.1:7897,lo0,Established,39,443,
 *
 * Connection rows split a process's bytes into loopback (to a local proxy → SysLoop)
 * and per-interface external traffic. Bytes of connections that closed between two
 * samples have no row; they're split like the process's last seen connections.
 */

import { spawn, type ChildProcess } from 'child_process';
import path from 'path';
import { createInterface } from 'readline';
import type { SourceStatus } from '../../../shared/types';
import { getSettings, updateSettings } from '../../services/settings';
import type { SysLoop, SysProc, SystemSample, SystemSource } from '../../services/sources/types';
import { ps } from './processes';
import { InterfaceTable } from './ifaces';

const NETTOP = '/usr/bin/nettop';
const ARGS = ['-L', '0', '-s', '1', '-d', '-x', '-n', '-J', 'interface,state,bytes_in,bytes_out'];
/** No sample for this long means nettop is stuck or gone. */
const SILENCE_MS = 6000;
/** Forget a process's connection mix after this many samples without one. */
const MIX_TTL = 60;

interface Addr {
  host: string | null;
  port: number | null;
}

export interface NtFlow {
  local: Addr;
  remote: Addr;
  iface: string;
  state: string;
  down: number;
  up: number;
}

export interface NtProc {
  /** As nettop prints it: truncated to ~15 characters */
  name: string;
  pid: number;
  down: number;
  up: number;
  flows: NtFlow[];
}

/** "192.168.1.2:443" (v4) / "fe80::1%en4.51016", "::1.27017" (v6) / "*:*" */
function parseAddr(s: string, v6: boolean): Addr {
  const i = s.lastIndexOf(v6 ? '.' : ':');
  if (i < 0) return { host: null, port: null };
  const host = s.slice(0, i).replace(/%.*$/, '');
  const port = s.slice(i + 1);
  return { host: host === '*' ? null : host, port: port === '*' ? null : Number(port) };
}

const FLOW = /^(tcp|udp)([46]) (.+)<->(.+)$/;

/** One sample (the lines between two headers). Exported for tests. */
export function parseSample(lines: string[]): NtProc[] {
  const out: NtProc[] = [];
  let cur: NtProc | null = null;
  for (const line of lines) {
    // Fields from the right: the name / connection itself may contain commas
    const cols = line.split(',');
    if (cols.length < 6) continue;
    const [iface, state, down, up] = cols.slice(-5, -1);
    const head = cols.slice(0, -5).join(',');
    const flow = head.match(FLOW);
    if (flow) {
      if (!cur) continue;
      const v6 = flow[2] === '6';
      cur.flows.push({
        local: parseAddr(flow[3], v6),
        remote: parseAddr(flow[4], v6),
        iface,
        state,
        down: Number(down) || 0,
        up: Number(up) || 0,
      });
      continue;
    }
    const proc = head.match(/^(.*)\.(\d+)$/);
    if (!proc) continue;
    cur = { name: proc[1], pid: Number(proc[2]), down: Number(down) || 0, up: Number(up) || 0, flows: [] };
    out.push(cur);
  }
  return out;
}

const isLoopback = (f: NtFlow) =>
  f.iface === 'lo0' || /^(127\.|::1$|::ffff:127\.)/.test(f.remote.host ?? f.local.host ?? '');

/** Where a process's bytes went in its last sample with open connections, as weights. */
interface Mix {
  /** key "<listener pid>:<port>" */
  loops: Map<string, { server: number; port: number; up: number; down: number }>;
  /** Loopback bytes that aren't to a known listener (server side of a local proxy…) */
  local: { up: number; down: number };
  ifs: Map<string, { up: number; down: number }>;
  hosts: Map<string, number>;
  age: number;
}

const total = (m: Mix) => {
  let up = m.local.up;
  let down = m.local.down;
  for (const v of m.loops.values()) (up += v.up), (down += v.down);
  for (const v of m.ifs.values()) (up += v.up), (down += v.down);
  return { up, down };
};

const cloneMix = (m: Mix): Mix => ({
  loops: new Map([...m.loops].map(([k, v]) => [k, { ...v }])),
  local: { ...m.local },
  ifs: new Map([...m.ifs].map(([k, v]) => [k, { ...v }])),
  hosts: new Map(m.hosts),
  age: 0,
});

export interface Attributed {
  procs: (Omit<SysProc, 'ifs'> & { pid: number; ifs: Map<string, [number, number]> })[];
  loops: { client: number; server: number; port: number; up: number; down: number }[];
}

/**
 * Turns successive samples into external bytes per process (by interface) and
 * loopback flows to listening processes. Processes are still pids here; the
 * source resolves them to paths. Exported for tests.
 */
export class Attributor {
  private mixes = new Map<number, Mix>();
  /** Loopback/wildcard listening TCP port -> pid */
  private listeners = new Map<number, number>();

  constructor(private defaultIface: () => string) {}

  add(sample: NtProc[]): Attributed {
    for (const p of sample) {
      for (const f of p.flows) {
        if (f.state === 'Listen' && f.local.port) this.listeners.set(f.local.port, p.pid);
      }
    }
    const procs: Attributed['procs'] = [];
    const loops = new Map<string, Attributed['loops'][number]>();
    const seen = new Set<number>();

    for (const p of sample) {
      seen.add(p.pid);
      const now: Mix = { loops: new Map(), local: { up: 0, down: 0 }, ifs: new Map(), hosts: new Map(), age: 0 };
      for (const f of p.flows) {
        if (!f.up && !f.down) continue;
        if (isLoopback(f)) {
          const server = f.remote.port != null ? this.listeners.get(f.remote.port) : undefined;
          // Our side is a client of another local process (app -> local proxy)
          if (server !== undefined && server !== p.pid && this.listeners.get(f.local.port ?? -1) !== p.pid) {
            const key = `${server}:${f.remote.port}`;
            const l = now.loops.get(key) ?? { server, port: f.remote.port!, up: 0, down: 0 };
            l.up += f.up;
            l.down += f.down;
            now.loops.set(key, l);
          } else {
            now.local.up += f.up;
            now.local.down += f.down;
          }
          continue;
        }
        const iface = f.iface || this.defaultIface();
        const i = now.ifs.get(iface) ?? { up: 0, down: 0 };
        i.up += f.up;
        i.down += f.down;
        now.ifs.set(iface, i);
        if (f.remote.host) now.hosts.set(f.remote.host, (now.hosts.get(f.remote.host) ?? 0) + f.up + f.down);
      }

      const seenBytes = total(now);
      const restUp = Math.max(0, p.up - seenBytes.up);
      const restDown = Math.max(0, p.down - seenBytes.down);
      const prev = this.mixes.get(p.pid);
      let basis = prev ?? null;
      if (seenBytes.up + seenBytes.down > 0) {
        basis = cloneMix(now);
        this.mixes.set(p.pid, basis);
      } else if (prev) {
        prev.age++;
      }
      if (restUp + restDown > 0) this.spread(now, basis, restUp, restDown);

      for (const l of now.loops.values()) {
        const key = `${p.pid}:${l.server}:${l.port}`;
        const acc = loops.get(key) ?? { client: p.pid, server: l.server, port: l.port, up: 0, down: 0 };
        acc.up += l.up;
        acc.down += l.down;
        loops.set(key, acc);
      }
      const ifs = new Map<string, [number, number]>();
      let up = 0;
      let down = 0;
      for (const [name, v] of now.ifs) {
        if (!v.up && !v.down) continue;
        ifs.set(name, [v.up, v.down]);
        up += v.up;
        down += v.down;
      }
      if (up + down === 0 && now.loops.size === 0) continue;
      procs.push({ pid: p.pid, path: '', up, down, hosts: [...now.hosts], ifs });
    }

    for (const [pid, m] of this.mixes) {
      if (!seen.has(pid) || m.age > MIX_TTL) this.mixes.delete(pid);
    }
    return { procs, loops: [...loops.values()] };
  }

  /** Book bytes of connections that closed unseen like `mix`, or as external on the default interface. */
  private spread(into: Mix, mix: Mix | null, up: number, down: number): void {
    const t = mix && total(mix);
    if (!mix || !t || t.up + t.down === 0) {
      const iface = this.defaultIface();
      const i = into.ifs.get(iface) ?? { up: 0, down: 0 };
      i.up += up;
      i.down += down;
      into.ifs.set(iface, i);
      return;
    }
    const share = (part: { up: number; down: number }) => ({
      up: t.up ? Math.round((up * part.up) / t.up) : 0,
      down: t.down ? Math.round((down * part.down) / t.down) : 0,
    });
    // A direction the mix never used still has to go somewhere: external
    const orphanUp = t.up ? 0 : up;
    const orphanDown = t.down ? 0 : down;
    for (const [key, l] of mix.loops) {
      const s = share(l);
      const acc = into.loops.get(key) ?? { server: l.server, port: l.port, up: 0, down: 0 };
      acc.up += s.up;
      acc.down += s.down;
      into.loops.set(key, acc);
    }
    const ifaces = mix.ifs.size ? mix.ifs : new Map([[this.defaultIface(), { up: 0, down: 0 }]]);
    let first = true;
    for (const [name, v] of ifaces) {
      const s = share(v);
      const acc = into.ifs.get(name) ?? { up: 0, down: 0 };
      acc.up += s.up + (first ? orphanUp : 0);
      acc.down += s.down + (first ? orphanDown : 0);
      into.ifs.set(name, acc);
      first = false;
    }
    for (const [h, n] of mix.hosts) {
      into.hosts.set(h, (into.hosts.get(h) ?? 0) + Math.round(((up + down) * n) / (t.up + t.down)));
    }
  }
}

/** pid -> executable path, from `ps`; refreshed when nettop shows a pid it doesn't know. */
class PathTable {
  private paths = new Map<number, string>();

  /** Look up these pids, re-reading the process table once if any is missing or reused. */
  async resolve(procs: { pid: number; name: string }[]): Promise<void> {
    const stale = procs.some((p) => {
      const known = this.paths.get(p.pid);
      return !known || !path.basename(known).startsWith(p.name);
    });
    if (!stale) return;
    try {
      const out = await ps(['-axww', '-o', 'pid=,comm=']);
      const next = new Map<number, string>();
      for (const line of out.split('\n')) {
        const m = line.match(/^\s*(\d+)\s+(.+)$/);
        if (m) next.set(Number(m[1]), m[2]);
      }
      this.paths = next;
    } catch {
      // keep the old table
    }
  }

  /** Full path, or nettop's (truncated) name for a process that's already gone. */
  get(pid: number, name: string): string {
    const known = this.paths.get(pid);
    return known && path.basename(known).startsWith(name) ? known : name;
  }
}

export class NettopSource implements SystemSource {
  readonly id = 'system';
  private child: ChildProcess | null = null;
  private lines: string[] = [];
  private headers = 0;
  private lastSampleAt = 0;
  private chain: Promise<void> = Promise.resolve();
  private ifaceTable = new InterfaceTable();
  private attributor = new Attributor(() => this.ifaceTable.defaultIface());
  private paths = new PathTable();
  private procs = new Map<string, SysProc>();
  private loops = new Map<string, SysLoop>();
  private enabled = getSettings().nettopEnabled;
  private lastError: string | null = null;

  constructor() {
    process.on('exit', () => this.child?.kill());
  }

  private get ready(): boolean {
    return !!this.child && this.lastSampleAt > 0 && Date.now() - this.lastSampleAt < SILENCE_MS;
  }

  async poll(): Promise<SystemSample> {
    if (!this.enabled) throw new Error('disabled');
    if (this.child && this.lastSampleAt && Date.now() - this.lastSampleAt > SILENCE_MS) this.stop();
    if (!this.child) this.start();
    if (!this.ready) throw new Error('nettop not ready');
    const sample: SystemSample = {
      procs: [...this.procs.values()],
      loops: [...this.loops.values()],
      ifaces: this.ifaceTable.list(),
    };
    this.procs.clear();
    this.loops.clear();
    return sample;
  }

  reset(): void {
    this.procs.clear();
    this.loops.clear();
  }

  async autoStart(): Promise<void> {
    if (this.enabled) this.start();
  }

  async toggle(): Promise<void> {
    this.enabled = !this.enabled;
    updateSettings({ nettopEnabled: this.enabled });
    if (this.enabled) this.start();
    else {
      this.stop();
      this.reset();
    }
  }

  private start(): void {
    if (this.child) return;
    this.lastError = null;
    this.lines = [];
    this.headers = 0;
    this.lastSampleAt = 0;
    this.ifaceTable.refresh();
    const child = spawn(NETTOP, ARGS, { stdio: ['ignore', 'pipe', 'ignore'] });
    this.child = child;
    child.on('error', (err) => {
      this.lastError = `无法运行 nettop：${err.message}`;
      if (this.child === child) this.child = null;
    });
    child.on('exit', () => {
      if (this.child === child) this.child = null;
    });
    createInterface({ input: child.stdout! }).on('line', (line) => this.onLine(line));
  }

  private stop(): void {
    this.child?.kill();
    this.child = null;
    this.lastSampleAt = 0;
  }

  private onLine(line: string): void {
    if (!line.startsWith(',')) {
      this.lines.push(line);
      return;
    }
    // A header starts the next sample, so the previous one is complete
    const lines = this.lines;
    this.lines = [];
    // Nothing precedes the first header; the first sample (completed by the second
    // header) holds totals since each process started, not deltas
    if (++this.headers <= 2) return;
    this.lastSampleAt = Date.now();
    const sample = parseSample(lines);
    this.chain = this.chain.then(() => this.ingest(sample)).catch(() => {});
  }

  private async ingest(sample: NtProc[]): Promise<void> {
    this.ifaceTable.refreshIfStale();
    const { procs, loops } = this.attributor.add(sample);
    const names = new Map(sample.map((p) => [p.pid, p.name]));
    const pids = new Set([...procs.map((p) => p.pid), ...loops.flatMap((l) => [l.client, l.server])]);
    const wanted = [...pids].map((pid) => ({ pid, name: names.get(pid) ?? '' }));
    await this.paths.resolve(wanted);
    const pathOf = (pid: number) => this.paths.get(pid, names.get(pid) ?? '');

    for (const p of procs) {
      const ifs: [number, number, number][] = [...p.ifs].map(([name, [up, down]]) => [this.ifaceTable.index(name), up, down]);
      this.addProc({ path: pathOf(p.pid), up: p.up, down: p.down, hosts: p.hosts, ifs });
    }
    for (const l of loops) this.addLoop({ client: pathOf(l.client), server: pathOf(l.server), port: l.port, up: l.up, down: l.down });
  }

  private addProc(p: SysProc): void {
    if (p.up + p.down === 0) return;
    const acc = this.procs.get(p.path);
    if (!acc) {
      this.procs.set(p.path, p);
      return;
    }
    acc.up += p.up;
    acc.down += p.down;
    acc.hosts.push(...p.hosts);
    for (const [index, up, down] of p.ifs ?? []) {
      const f = acc.ifs?.find((x) => x[0] === index);
      if (f) {
        f[1] += up;
        f[2] += down;
      } else {
        (acc.ifs ??= []).push([index, up, down]);
      }
    }
  }

  private addLoop(l: SysLoop): void {
    if (l.up + l.down === 0) return;
    const key = `${l.client}\0${l.server}\0${l.port}`;
    const acc = this.loops.get(key);
    if (!acc) {
      this.loops.set(key, l);
      return;
    }
    acc.up += l.up;
    acc.down += l.down;
  }

  status(): SourceStatus {
    const base = { id: this.id, label: '系统流量' };
    if (!this.enabled) return { ...base, state: 'off', detail: '已停用', action: '启用' };
    if (this.lastError) return { ...base, state: 'error', detail: this.lastError, action: null };
    if (this.ready) return { ...base, state: 'on', detail: '已启用（nettop）', action: '停用' };
    return { ...base, state: 'off', detail: '正在启动…', action: null };
  }
}

