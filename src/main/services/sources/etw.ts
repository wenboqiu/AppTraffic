/**
 * Traffic source backed by traffic-etw.exe, an elevated helper that counts every
 * process's TCP/UDP bytes via ETW and streams per-second deltas over a named pipe.
 * The helper exits by itself ~30 s after its last client disconnects, so all this
 * side does is start it (one UAC prompt) and keep reading.
 */

import { app } from 'electron';
import net from 'net';
import path from 'path';
import { existsSync } from 'fs';
import { execFile } from 'child_process';
import type { SourceStatus } from '../../../shared/types';
import type { EtwIface, EtwLoop, EtwProc, EtwSample, TrafficSource } from './types';

const PIPE = '\\\\.\\pipe\\apptraffic-etw';
const EXE = 'traffic-etw.exe';
/** No line (not even the 2 s keep-alive) for this long means the helper is gone. */
const SILENCE_MS = 6000;

interface Line {
  procs?: EtwProc[];
  loops?: EtwLoop[];
  ifaces?: EtwIface[];
}

export function helperPath(): string {
  return app.isPackaged
    ? path.join(process.resourcesPath, EXE)
    : path.join(app.getAppPath(), 'native', 'target', 'release', EXE);
}

export class EtwSource implements TrafficSource<EtwSample> {
  readonly id = 'etw';
  private sock: net.Socket | null = null;
  private ready = false;
  private lastLineAt = 0;
  private buf = '';
  private procs = new Map<string, EtwProc>();
  private loops = new Map<string, EtwLoop>();
  /** Latest description of every interface seen (index -> info) */
  private ifaces = new Map<number, EtwIface>();
  private starting = false;
  private lastError: string | null = null;

  async poll(): Promise<EtwSample> {
    if (this.ready && Date.now() - this.lastLineAt > SILENCE_MS) this.drop();
    if (!this.ready) {
      this.connect();
      throw new Error('helper not connected');
    }
    const sample: EtwSample = {
      procs: [...this.procs.values()],
      loops: [...this.loops.values()],
      ifaces: [...this.ifaces.values()],
    };
    this.procs.clear();
    this.loops.clear();
    return sample;
  }

  reset(): void {
    this.procs.clear();
    this.loops.clear();
  }

  /** Launch the helper elevated. Resolves once the UAC prompt is answered. */
  start(): Promise<void> {
    const exe = helperPath();
    if (!existsSync(exe)) {
      this.lastError = `找不到 ${EXE}`;
      return Promise.resolve();
    }
    if (this.starting || this.ready) return Promise.resolve();
    this.starting = true;
    this.lastError = null;
    // Start-Process -Verb RunAs is the shell's "run as administrator"; it fails
    // (exit code 1) when the user declines the UAC prompt.
    const cmd = `Start-Process -FilePath '${exe.replace(/'/g, "''")}' -Verb RunAs`;
    return new Promise((resolve) => {
      execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', cmd], { windowsHide: true }, (err) => {
        this.starting = false;
        if (err) this.lastError = '未获得管理员授权，系统流量统计未启动';
        resolve();
      });
    });
  }

  private connect(): void {
    if (this.sock) return;
    const sock = net.connect(PIPE);
    this.sock = sock;
    sock.setEncoding('utf8');
    sock.on('data', (chunk: string) => this.onData(chunk));
    // ENOENT just means the helper isn't running; try again next tick
    sock.on('error', () => this.drop());
    sock.on('close', () => this.drop());
  }

  private drop(): void {
    this.sock?.destroy();
    this.sock = null;
    this.ready = false;
    this.buf = '';
  }

  private onData(chunk: string): void {
    this.buf += chunk;
    let i: number;
    while ((i = this.buf.indexOf('\n')) >= 0) {
      const raw = this.buf.slice(0, i);
      this.buf = this.buf.slice(i + 1);
      let line: Line;
      try {
        line = JSON.parse(raw) as Line;
      } catch {
        continue;
      }
      this.ready = true;
      this.lastLineAt = Date.now();
      for (const p of line.procs ?? []) this.addProc(p);
      for (const l of line.loops ?? []) this.addLoop(l);
      for (const f of line.ifaces ?? []) this.ifaces.set(f.index, f);
    }
  }

  private addProc(p: EtwProc): void {
    const acc = this.procs.get(p.path);
    if (!acc) {
      this.procs.set(p.path, { ...p, hosts: [...p.hosts], ifs: (p.ifs ?? []).map((f) => [...f]) });
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

  private addLoop(l: EtwLoop): void {
    const key = `${l.client}\0${l.server}\0${l.port}`;
    const acc = this.loops.get(key);
    if (!acc) {
      this.loops.set(key, { ...l });
      return;
    }
    acc.up += l.up;
    acc.down += l.down;
  }

  status(): SourceStatus {
    const base = { id: this.id, label: '系统流量' };
    if (this.ready) return { ...base, state: 'on', detail: '已启用', action: null };
    if (this.starting) return { ...base, state: 'off', detail: '等待管理员授权…', action: null };
    if (this.lastError) return { ...base, state: 'error', detail: this.lastError, action: '启用' };
    return { ...base, state: 'off', detail: '未启用：按应用统计全部流量需要管理员授权', action: '启用' };
  }
}
