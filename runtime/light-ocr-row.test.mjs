import assert from 'node:assert/strict'
import { existsSync, readFileSync, realpathSync } from 'node:fs'
import path from 'node:path'
import { test } from 'node:test'
import { load } from '@arcships/rutis/testing'
import row from './plugins/light-ocr/index.ts'

// The Node root the runtime process runs under, and the plugin directory the
// row resolves: both real paths, as the helper's read grant needs them.
const pluginDirectory = realpathSync(path.join(import.meta.dirname, 'plugins', 'light-ocr'))
const manifest = JSON.parse(readFileSync(path.join(pluginDirectory, 'dim-plugin.json'), 'utf8'))

process.env.RUTIS_HOST ??= path.join(import.meta.dirname, 'node_modules', '.bin',
  process.platform === 'win32' ? 'rutis-host.cmd' : 'rutis-host')

test('the row describes the PP-OCR helper to Modality', async () => {
  const loaded = await load(row, { config: {} })
  assert.deepEqual(loaded.provided(), ['ocrHelper'])
  assert.deepEqual(loaded.methods('ocrHelper'), { describe: 'async' })
  assert.deepEqual(row.provides.ocrHelper, { describe: 'async' })
  const description = await loaded.service('ocrHelper').describe()
  await loaded.unload()

  // What `dim_ocr::HelperDescription` reads, and nothing else: its two layers
  // both deny unknown fields.
  assert.deepEqual(Object.keys(description).sort(), ['args', 'contract', 'executable', 'read', 'target'])
  assert.equal(description.contract, 1)
  assert.deepEqual(description.target, { providerId: 'local', modelId: 'pp-ocr' })

  // The running Node binary, and the root `node` belongs to: `bin/node` on
  // POSIX, `node.exe` in the installation directory on Windows.
  const executable = realpathSync(description.executable)
  assert.ok(path.isAbsolute(description.executable), description.executable)
  assert.ok(['node', 'node.exe'].includes(path.basename(description.executable)), description.executable)
  assert.equal(path.relative(description.read[0], executable),
    process.platform === 'win32' ? 'node.exe' : path.join('bin', 'node'))

  // The helper beside the row, and the plugin directory it lives in.
  assert.deepEqual(description.args, [path.join(pluginDirectory, 'helper.cjs')])
  assert.ok(existsSync(description.args[0]), description.args[0])
  assert.deepEqual(description.read, [description.read[0], pluginDirectory])
})

test('the manifest declares exactly what the row provides', () => {
  assert.equal(manifest.name, 'light-ocr')
  assert.equal(manifest.code.entry, 'index.ts')
  assert.deepEqual(manifest.code.provides, row.provides)
  assert.equal(manifest.dependencies, 'npm')
  assert.match(manifest.version, /^\d+\.\d+\.\d+$/)
})
