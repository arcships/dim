import { definePlugin } from '@arcships/rutis'
import { realpathSync } from 'node:fs'
import path from 'node:path'

const provides = { ocrHelper: { describe: 'async' } } as const

/** The installation root the running Node binary belongs to: the directory
 * above `bin/node` on POSIX, and the binary's own directory on Windows. */
function nodeRoot(executable: string): string {
  const directory = path.dirname(executable)
  return process.platform === 'win32' || path.basename(directory) !== 'bin'
    ? directory
    : path.dirname(directory)
}

export default definePlugin({
  name: 'LightOcr',
  provides,
  apply(ctx) {
    const pluginDirectory = realpathSync(import.meta.dirname)
    const executable = process.execPath
    const read = [nodeRoot(realpathSync(executable)), pluginDirectory]
    const args = [path.join(pluginDirectory, 'helper.cjs')]
    ctx.provide('ocrHelper', {
      async describe() {
        return {
          contract: 1,
          target: { providerId: 'local', modelId: 'pp-ocr' },
          executable,
          args,
          read,
        }
      },
    })
  },
})
