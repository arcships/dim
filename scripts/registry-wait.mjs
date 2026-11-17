import assert from 'node:assert/strict';

function milliseconds(name, fallback) {
  const value = process.env[name];
  if (value === undefined) return fallback;
  const parsed = Number(value);
  assert(Number.isFinite(parsed) && parsed >= 0, `${name} must be a non-negative number of milliseconds: ${value}`);
  return parsed;
}

/**
 * How long the nightly scripts keep asking a registry, and how often. A
 * publication is accepted before it is served: its version document, its
 * tarball and its dist-tags become readable at different moments, and never at
 * the instant a write returns. Every wait in the publication checks and the
 * checker is bounded by these two, and a caller may shorten them, which the
 * flow tests do.
 */
export const settleMilliseconds = milliseconds('DIM_REGISTRY_SETTLE_MS', 600_000);
export const pollMilliseconds = milliseconds('DIM_REGISTRY_POLL_MS', 15_000);
