/**
 * Starting the elevated ETW helper.
 *
 * Preferred: a scheduled task "\AppTraffic\ETW Collector" (run level highest) that
 * the current user may run without a prompt. Registering it takes one UAC prompt,
 * done by resources/etw-task.ps1 when the user enables system-wide statistics.
 * Because the task runs elevated, it is only ever pointed at a helper inside
 * Program Files, where a non-admin can't swap the executable. Anywhere else (dev
 * builds, per-user installs) the helper is started with a UAC prompt each time.
 */

import { app } from 'electron';
import path from 'path';
import { execFile } from 'child_process';

const TASK_PATH = '\\AppTraffic\\';
const TASK_NAME = 'ETW Collector';

export function runPowerShell(script: string, timeoutMs = 15_000): Promise<string> {
  const encoded = Buffer.from(`[Console]::OutputEncoding = [Text.Encoding]::UTF8\n${script}`, 'utf16le').toString(
    'base64',
  );
  return new Promise((resolve, reject) => {
    execFile(
      'powershell.exe',
      ['-NoProfile', '-NonInteractive', '-EncodedCommand', encoded],
      // The UAC prompt can sit there for a while; callers that wait on it pass 0
      { windowsHide: true, timeout: timeoutMs, maxBuffer: 4 * 1024 * 1024 },
      (err, stdout) => (err ? reject(err) : resolve(stdout)),
    );
  });
}

/** PowerShell single-quoted string literal */
function psq(s: string): string {
  return `'${s.replace(/'/g, "''")}'`;
}

export function taskScriptPath(): string {
  return app.isPackaged
    ? path.join(process.resourcesPath, 'resources', 'etw-task.ps1')
    : path.join(app.getAppPath(), 'resources', 'etw-task.ps1');
}

/** Inside Program Files, i.e. only administrators can replace it. */
export function isProtectedPath(p: string): boolean {
  const full = path.resolve(p).toLowerCase();
  return [process.env.ProgramFiles, process.env['ProgramFiles(x86)'], process.env.ProgramW6432]
    .filter((d): d is string => !!d)
    .some((d) => full.startsWith(path.resolve(d).toLowerCase() + path.sep));
}

/** Executable the scheduled task runs, or null when there's no task (or it can't be read). */
export async function taskExecutable(): Promise<string | null> {
  try {
    const out = await runPowerShell(
      `$t = Get-ScheduledTask -TaskPath ${psq(TASK_PATH)} -TaskName ${psq(TASK_NAME)} -ErrorAction Stop
$t.Actions[0].Execute`,
    );
    return out.trim() || null;
  } catch {
    return null;
  }
}

export function samePath(a: string, b: string): boolean {
  return path.resolve(a.replace(/^"|"$/g, '')).toLowerCase() === path.resolve(b).toLowerCase();
}

/** Start the task without a prompt (the task grants the user run access). */
export function runTask(): Promise<boolean> {
  return new Promise((resolve) => {
    execFile('schtasks.exe', ['/Run', '/TN', TASK_PATH + TASK_NAME], { windowsHide: true }, (err) => resolve(!err));
  });
}

/**
 * Run a program elevated (UAC prompt). Resolves false if the prompt is declined.
 * With wait, resolves after the program exits and only if it exited with 0.
 */
export async function runElevated(file: string, args: string[], wait: boolean): Promise<boolean> {
  // Start-Process joins ArgumentList with spaces, so quote arguments that contain them
  const list = args.map((a) => psq(/\s/.test(a) ? `"${a}"` : a)).join(',');
  const argPart = args.length ? ` -ArgumentList @(${list})` : '';
  const script = wait
    ? `$p = Start-Process -FilePath ${psq(file)}${argPart} -Verb RunAs -WindowStyle Hidden -PassThru -Wait
exit $p.ExitCode`
    : `Start-Process -FilePath ${psq(file)}${argPart} -Verb RunAs`;
  try {
    await runPowerShell(script, 0);
    return true;
  } catch {
    return false;
  }
}

/** Register the task for `exe` and start it (one UAC prompt). */
export function registerAndRunTask(exe: string): Promise<boolean> {
  return runElevated(
    'powershell.exe',
    ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', taskScriptPath(), '-ExePath', exe, '-Run'],
    true,
  );
}

/** Remove the task (one UAC prompt). */
export function unregisterTask(): Promise<boolean> {
  return runElevated(
    'powershell.exe',
    ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', taskScriptPath(), '-Unregister'],
    true,
  );
}
