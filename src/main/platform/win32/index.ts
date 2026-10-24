import type { Platform } from '../types';
import { knownEndpoints, listProcesses, socketEndpoints } from './processes';
import { systemProxyPorts } from './sysproxy';
import { EtwSource } from './etw';

export const win32: Platform = {
  listProcesses,
  knownEndpoints,
  socketEndpoints,
  systemProxyPorts,
  createSystemSource: () => new EtwSource(),
};
