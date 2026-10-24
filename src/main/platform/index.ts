import type { Platform } from './types';
import { win32 } from './win32';
import { darwin } from './darwin';

/** The current OS's implementation. */
export const platform: Platform = process.platform === 'win32' ? win32 : darwin;
