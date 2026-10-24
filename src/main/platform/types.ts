/**
 * What the rest of the app needs from the operating system. One implementation
 * per platform (win32/, darwin/); platform/index.ts picks the current one.
 */

import type { Endpoint } from '../services/mihomo';
import type { SystemSource } from '../services/sources/types';

export interface CoreProc {
  pid: number;
  /** Executable file name ("verge-mihomo.exe", "mihomo") */
  name: string;
  /** Parent process's executable file name; names the GUI client */
  parent: string | null;
  /** Command line split into arguments (argv[0] first); null if it can't be read */
  args: string[] | null;
}

export interface Platform {
  /** Running processes whose executable name (without .exe, case-insensitive) is in `names`. */
  listProcesses(names: string[]): Promise<CoreProc[]>;
  /**
   * Controller endpoints from well-known client config locations, for cores whose
   * command line can't be read (e.g. running as a service / another user).
   */
  knownEndpoints(): Endpoint[];
  /** Local sockets (named pipes / Unix sockets) that look like a core's controller. */
  socketEndpoints(): Endpoint[];
  /** Loopback ports the system proxy currently points at. */
  systemProxyPorts(): Set<number>;
  /** System-wide per-process traffic (Windows: ETW helper; macOS: nettop). */
  createSystemSource(): SystemSource;
}
