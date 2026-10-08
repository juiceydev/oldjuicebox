import { PARALLEL_STREAMS } from "./upload-config";

let inUse = 0;

export function tryAcquireStream(): boolean {
  if (inUse >= PARALLEL_STREAMS) return false;
  inUse++;
  return true;
}

export function releaseStream(): void {
  inUse = Math.max(0, inUse - 1);
}
