import type { Platform } from '../types';
import { knownEndpoints, listProcesses, socketEndpoints } from './processes';
import { systemProxyPorts } from './sysproxy';
import { NettopSource } from './nettop';

export const darwin: Platform = {
  listProcesses,
  knownEndpoints,
  socketEndpoints,
  systemProxyPorts,
  createSystemSource: () => new NettopSource(),
};
