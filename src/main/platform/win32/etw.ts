/**
 * Traffic source backed by traffic-etw.exe, an elevated helper that counts every
 * process's TCP/UDP bytes via ETW and streams per-second deltas over a named pipe.
 * The helper exits by itself ~30 s after its last client disconnects, so all this
 * side does is start it (see elevate.ts) and keep reading.
 */

import { app } from 'electron';
import net from 'net';
import path from 'path';
import { existsSync } from 'fs';
import type { SourceStatus } from '../../../shared/types';
import { getSettings, updateSettings } from '../../services/settings';
import {
  isProtectedPath,
  registerAndRunTask,
  runElevated,
  runTask,
  samePath,
  taskExecutable,
  unregisterTask,
} from './elevate';
import type { SysIface, SysLoop, SysProc, SystemSample, SystemSource } from '../../services/sources/types';

const PIPE = '\\\\.\\pipe\\apptraffic-etw';
const EXE = 'traffic-etw.exe';
/** No line (not even the 2 s keep-alive) for this long means the helper is gone. */
const SILENCE_MS = 6000;
/** After launching, how long "starting" is shown before it counts as failed. */
const START_GRACE_MS = 15_000;

interface Line {
  procs?: SysProc[];
  loops?: SysLoop[];
  ifaces?: SysIface[];
}

export function helperPath(): string {
  return app.isPackaged
    ? path.join(process.resourcesPath, EXE)
    : path.join(app.getAppPath(), 'native', 'target', 'release', EXE);
}

export class EtwSource implements SystemSource {
  readonly id = 'system';
  private sock: net.Socket | null = null;
  private ready = false;
  private lastLineAt = 0;
  private buf = '';
  private procs = new Map<string, SysProc>();
  private loops = new Map<string, SysLoop>();
  /** Latest description of every interface seen (index -> info) */
  private ifaces = new Map<number, SysIface>();
  /** The user wants system-wide statistics (persisted). */
  private enabled = getSettings().etwEnabled;
  /** Waiting on a UAC prompt or a scheduled-task start */
  private busy = false;
  /** A UAC prompt is (about to be) on screen */
  private prompting = false;
  private launchedAt = 0;
  private lastError: string | null = null;

  async poll(): Promise<SystemSample> {
    if (this.ready && Date.now() - this.lastLineAt > SILENCE_MS) this.drop();
    if (!this.ready) {
      if (this.enabled) this.connect();
      throw new Error('helper not connected');
    }
    const sample: SystemSample = {
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

  /** On app start: bring the helper back if the user enabled it, without prompting. */
  async autoStart(): Promise<void> {
    if (this.enabled) await this.launch(false);
  }

  /** The source's button: enable (may prompt for UAC once) or disable. */
  async toggle(): Promise<void> {
    if (this.busy) return;
    if (this.enabled && (this.ready || this.starting())) await this.disable();
    else await this.launch(true);
  }

  private starting(): boolean {
    return this.busy || (this.enabled && Date.now() - this.launchedAt < START_GRACE_MS);
  }

  /**
   * Start the helper: silently through the scheduled task when it exists and points
   * at this helper; otherwise (interactive only) register the task with one UAC
   * prompt, or for an unprotected helper path just run it elevated.
   */
  private async launch(interactive: boolean): Promise<void> {
    const exe = helperPath();
    if (!existsSync(exe)) {
      this.lastError = `找不到 ${EXE}`;
      return;
    }
    this.busy = true;
    this.lastError = null;
    try {
      let ok = false;
      if (isProtectedPath(exe)) {
        const current = await taskExecutable();
        ok = !!current && samePath(current, exe) && (await runTask());
        // Missing, or left over from an install elsewhere: (re)register it
        if (!ok && interactive) {
          this.prompting = true;
          ok = await registerAndRunTask(exe);
        }
      } else if (interactive) {
        this.prompting = true;
        ok = await runElevated(exe, [], false);
      }
      if (ok) {
        this.enabled = true;
        this.launchedAt = Date.now();
        updateSettings({ etwEnabled: true });
      } else if (interactive) {
        this.lastError = '未获得管理员授权，系统流量统计未启动';
      }
    } finally {
      this.busy = this.prompting = false;
    }
  }

  /** Stop collecting and remove the scheduled task (one UAC prompt if it exists). */
  private async disable(): Promise<void> {
    this.busy = true;
    try {
      this.enabled = false;
      updateSettings({ etwEnabled: false });
      // The helper exits on its own once nobody is connected
      this.drop();
      this.reset();
      if (await taskExecutable()) {
        this.prompting = true;
        await unregisterTask();
      }
    } finally {
      this.busy = this.prompting = false;
    }
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

  private addProc(p: SysProc): void {
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

  private addLoop(l: SysLoop): void {
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
    if (this.ready) return { ...base, state: 'on', detail: '已启用', action: '停用' };
    if (this.prompting) return { ...base, state: 'off', detail: '请在管理员授权窗口中确认…', action: null };
    if (this.starting()) return { ...base, state: 'off', detail: '正在启动…', action: null };
    if (this.lastError) return { ...base, state: 'error', detail: this.lastError, action: '启用' };
    if (this.enabled) return { ...base, state: 'error', detail: '采集程序未运行', action: '启用' };
    return { ...base, state: 'off', detail: '未启用：按应用统计全部流量需要管理员授权', action: '启用' };
  }
}
