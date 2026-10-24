/**
 * macOS: running processes with their parent and command line (`ps`), plus the
 * places Clash clients keep their controller.
 */

import os from 'os';
import path from 'path';
import { execFile } from 'child_process';
import { existsSync, readdirSync, statSync } from 'fs';
import { endpointsFromConfig, readControllerConfig } from '../../services/controller-config';
import type { Endpoint } from '../../services/mihomo';
import type { CoreProc } from '../types';

const SOCKET_HINT = /mihomo|clash|verge|sing-?box|sparkle|party/i;
/** Where clients put their controller socket */
const SOCKET_DIRS = ['/tmp', '/tmp/verge', os.tmpdir()];
const APP_SUPPORT = path.join(os.homedir(), 'Library', 'Application Support');

export function ps(args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile('/bin/ps', args, { maxBuffer: 16 * 1024 * 1024 }, (err, stdout) => (err ? reject(err) : resolve(stdout)));
  });
}

/** "  754   555 /Applications/X.app/Contents/MacOS/x" -> [754, 555, path] */
function parseRow(line: string): [number, number, string] | null {
  const m = line.match(/^\s*(\d+)\s+(\d+)\s+(.+)$/);
  return m ? [Number(m[1]), Number(m[2]), m[3]] : null;
}

/**
 * Split `ps -o args` output back into arguments. ps joins argv with spaces, so a
 * path with spaces can't be told apart from two arguments; but argv[0] is known
 * (it's the executable) and values follow flags, so each "-flag value" pair is cut
 * at the next " -flag". Good enough for core command lines.
 */
export function splitArgs(cmd: string, exe: string): string[] {
  let rest = cmd;
  const out: string[] = [];
  if (rest.startsWith(exe)) {
    out.push(exe);
    rest = rest.slice(exe.length);
  }
  for (const chunk of rest.split(/\s+(?=--?[A-Za-z][\w-]*(?:\s|=|$))/)) {
    const c = chunk.trim();
    if (!c) continue;
    const m = c.match(/^(--?[A-Za-z][\w-]*)(?:[=\s]+(.*))?$/);
    if (m) {
      out.push(m[1]);
      if (m[2] !== undefined) out.push(m[2]);
    } else {
      out.push(c);
    }
  }
  return out;
}

export async function listProcesses(names: string[]): Promise<CoreProc[]> {
  const wanted = new Set(names.map((n) => n.toLowerCase()));
  let table: string;
  try {
    table = await ps(['-axww', '-o', 'pid=,ppid=,comm=']);
  } catch {
    return [];
  }
  const rows = table.split('\n').map(parseRow).filter((r): r is [number, number, string] => !!r);
  const exeOf = new Map(rows.map(([pid, , exe]) => [pid, exe]));
  const cores = rows.filter(([, , exe]) => wanted.has(path.basename(exe).toLowerCase()));
  return Promise.all(
    cores.map(async ([pid, ppid, exe]) => {
      let args: string[] | null = null;
      try {
        const cmd = (await ps(['-ww', '-o', 'args=', '-p', String(pid)])).trim();
        if (cmd) args = splitArgs(cmd, exe);
      } catch {
        // exited meanwhile
      }
      const parent = exeOf.get(ppid);
      return { pid, name: path.basename(exe), parent: parent ? path.basename(parent) : null, args };
    }),
  );
}

/** Clash Verge Rev's config, for when its core runs as a service. */
export function knownEndpoints(): Endpoint[] {
  const file = path.join(APP_SUPPORT, 'io.github.clash-verge-rev.clash-verge-rev', 'clash-verge.yaml');
  if (!existsSync(file)) return [];
  const eps = endpointsFromConfig(file, 'Clash Verge');
  if (!eps.some((e) => e.kind === 'socket')) {
    const secret = readControllerConfig(file).secret ?? '';
    eps.unshift({ kind: 'socket', socketPath: '/tmp/verge/verge-mihomo.sock', secret, client: 'Clash Verge' });
  }
  return eps;
}

export function socketEndpoints(): Endpoint[] {
  const out: Endpoint[] = [];
  for (const dir of new Set(SOCKET_DIRS)) {
    let names: string[];
    try {
      names = readdirSync(dir);
    } catch {
      continue;
    }
    for (const n of names) {
      if (!SOCKET_HINT.test(n) || !n.endsWith('.sock')) continue;
      const socketPath = path.join(dir, n);
      try {
        if (!statSync(socketPath).isSocket()) continue;
      } catch {
        continue;
      }
      const client = /verge/i.test(n) ? 'Clash Verge' : /party/i.test(n) ? 'Mihomo Party' : /sparkle/i.test(n) ? 'Sparkle' : n;
      out.push({ kind: 'socket', socketPath, secret: '', client });
    }
  }
  return out;
}
