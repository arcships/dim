// The test's side of a plugin the test tool runs in another process
// (rutis-0.19-upgrade M10). `driveCallback` calls a function a plugin handed the
// test, from the runtime and asynchronously: a function the test calls itself is
// a synchronous session call, and the plugin cannot await a service of the test
// inside one. `counted` and `flush` are the test's other sign of the work the
// plugin continues after the call that drove it returns.
import { type Plugin, definePlugin } from '@arcships/rutis'

const activity = { calls: 0, pending: 0 }

/** Starts the count for a fixture: what the plugin had done before it began. */
export function measure() {
  activity.calls = 0
  activity.pending = 0
}

/** One test service, telling `activity` as the plugin calls its methods. */
export function counted(
  methods: Record<string, (...args: any[]) => unknown>,
): Record<string, (...args: any[]) => Promise<unknown>> {
  return Object.fromEntries(Object.entries(methods).map(([name, fn]) => [name, async (...args: unknown[]) => {
    activity.calls++
    activity.pending++
    try { return await fn(...args) } finally { activity.pending-- }
  }]))
}

/** Waits until no call of the plugin's is out and none has been made for four turns, and at least 20 ms. */
export async function flush(): Promise<void> {
  const began = Date.now()
  let seen = activity.calls
  for (let index = 0, quiet = 0; index < 2000 && (quiet < 4 || Date.now() - began < 20); index++) {
    await new Promise(resolve => setTimeout(resolve, 1))
    quiet = activity.pending === 0 && activity.calls === seen ? quiet + 1 : 0
    seen = activity.calls
  }
}

export default definePlugin({
  name: 'driveCallback',
  async apply(ctx) {
    ctx.provide('driveCallback', {
      async call(fn: (...args: unknown[]) => unknown, args: unknown[]) {
        return await fn(...args)
      },
    })
  },
}) as Plugin<{}>
