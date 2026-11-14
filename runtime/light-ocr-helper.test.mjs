import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, truncateSync, writeFileSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { test } from 'node:test'

const runtimeRoot = import.meta.dirname
const pluginSource = path.join(runtimeRoot, 'plugins', 'light-ocr')
const fixtures = path.join(runtimeRoot, 'fixtures', 'light-ocr')

// dim_ocr::MAX_PAGE_PIXELS
const PAGE_PIXEL_BOUND = 25_000_000

const lock = JSON.parse(readFileSync(path.join(pluginSource, 'package-lock.json'), 'utf8'))

function identity(entry) {
  const os = { darwin: 'macos', win32: 'windows' }[entry.os[0]] ?? entry.os[0]
  return entry.libc?.[0] === 'musl' ? `${os}-${entry.cpu[0]}-musl` : `${os}-${entry.cpu[0]}`
}

const platformPackages = Object.entries(lock.packages)
  .filter(([name, entry]) => name !== '' && Array.isArray(entry.os) && Array.isArray(entry.cpu))
  .map(([name, entry]) => ({ identity: identity(entry), name: name.replace(/^node_modules\//, '') }))

const PNG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00])
const JPEG = Buffer.from([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10])
const PDF = Buffer.from('%PDF-1.7\nfixture document\n%%EOF\n')

const BOX = [{ x: 10, y: 20 }, { x: 110, y: 20 }, { x: 110, y: 60 }, { x: 10, y: 60 }]
const LINES = [
  { text: 'DIMCODE', confidence: 0.93, box: BOX },
  { text: 'OCR', confidence: 1.4, box: BOX },
  { text: 'low', confidence: -0.2, box: BOX },
]
const REGIONS = [
  { text: 'DIMCODE', confidence: 0.93, polygon: [[10, 20], [110, 20], [110, 60], [10, 60]] },
  { text: 'OCR', confidence: 1, polygon: [[10, 20], [110, 20], [110, 60], [10, 60]] },
  { text: 'low', confidence: 0, polygon: [[10, 20], [110, 20], [110, 60], [10, 60]] },
]
const PAGES = [
  { index: 0, width: 1000, height: 1400, lines: [{ text: 'page one', confidence: 0.9, box: BOX }] },
  { index: 1, width: 1000, height: 1400, lines: [{ text: 'page two', confidence: 0.8, box: BOX }] },
]
const PAGE_REGION = [[10, 20], [110, 20], [110, 60], [10, 60]]

// A plugin tree: the helper beside the fixture's `node_modules`, in a
// temporary directory. The platform packages exist only when a test needs
// the PDF renderer, so a text-mode run that resolves one fails loudly.
function makeContext(t, { platform = false } = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'dim-light-ocr-'))
  t.after(() => rmSync(root, { recursive: true, force: true }))
  const plugin = path.join(root, 'plugin')
  const artifacts = path.join(root, 'artifacts')
  mkdirSync(plugin, { recursive: true })
  mkdirSync(artifacts, { recursive: true })
  cpSync(path.join(pluginSource, 'helper.cjs'), path.join(plugin, 'helper.cjs'))
  const modules = path.join(plugin, 'node_modules', '@arcships')
  cpSync(path.join(fixtures, 'light-ocr'), path.join(modules, 'light-ocr'), { recursive: true })
  cpSync(path.join(fixtures, 'light-ocr-runtime'), path.join(modules, 'light-ocr-runtime'), { recursive: true })
  if (platform) {
    for (const { name } of platformPackages) {
      const directory = path.join(modules, name.split('/')[1])
      mkdirSync(path.join(directory, 'pdfium'), { recursive: true })
      writeFileSync(path.join(directory, 'package.json'), `${JSON.stringify({ name, version: '0.5.9', exports: { './pdfium': './pdfium/index.cjs' } })}\n`)
      cpSync(path.join(fixtures, 'pdfium', 'index.cjs'), path.join(directory, 'pdfium', 'index.cjs'))
    }
  }
  return { root, plugin, artifacts, helper: path.join(plugin, 'helper.cjs') }
}

function writeFile(directory, name, data) {
  const file = path.join(directory, name)
  writeFileSync(file, data)
  return file
}

// Run the helper in a child process with the fixture's plan and log.
function run(context, args, settings) {
  const plan = path.join(context.root, 'plan.json')
  const log = path.join(context.root, 'log.jsonl')
  writeFileSync(plan, JSON.stringify(settings))
  writeFileSync(log, '')
  const child = spawnSync(process.execPath, [context.helper, ...args], {
    encoding: 'utf8',
    env: { ...process.env, LIGHT_OCR_FIXTURE_PLAN: plan, LIGHT_OCR_FIXTURE_LOG: log },
  })
  const lines = child.stdout.split('\n').filter(line => line !== '')
  return {
    status: child.status,
    stdout: child.stdout,
    stderr: child.stderr,
    lines,
    reply: lines.length === 1 ? JSON.parse(lines[0]) : undefined,
    calls: readFileSync(log, 'utf8').split('\n').filter(line => line !== '').map(line => JSON.parse(line)),
  }
}

function succeeded(result) {
  assert.equal(result.status, 0, result.stdout + result.stderr)
  assert.equal(result.stderr, '')
  assert.equal(result.lines.length, 1, result.stdout)
  assert.equal(result.reply.ok, true)
  return result.reply
}

function refused(result, code) {
  assert.equal(result.status, 2, result.stdout + result.stderr)
  assert.equal(result.stderr, '')
  assert.equal(result.lines.length, 1, result.stdout)
  assert.equal(result.reply.ok, false)
  assert.equal(result.reply.error.code, code, result.reply.error.message)
  return result.reply.error
}

function artifactsOf(context) {
  return readdirSync(context.artifacts).sort()
}

function document(context) {
  return JSON.parse(readFileSync(path.join(context.artifacts, 'result.json'), 'utf8'))
}

// Every file below `directory` with its size, sorted.
function listing(directory) {
  const entries = []
  const walk = (current, prefix) => {
    for (const name of readdirSync(current).sort()) {
      const file = path.join(current, name)
      const metadata = statSync(file)
      if (metadata.isDirectory()) {
        entries.push(`${prefix}${name}/`)
        walk(file, `${prefix}${name}/`)
      } else {
        entries.push(`${prefix}${name} ${metadata.size}`)
      }
    }
  }
  walk(directory, '')
  return entries
}

function layoutArguments(context, input) {
  return ['--input', input, '--artifacts', context.artifacts, '--output-mode', 'layout']
}

test('the helper refuses a bad command line', t => {
  const context = makeContext(t)
  const input = writeFile(context.root, 'input.png', PNG)
  const cases = [
    [[], '--input is required'],
    [['--artifacts', context.artifacts], '--input is required'],
    [['--input', input], '--artifacts is required'],
    [['--input'], '--input requires a value'],
    [['--input', input, '--artifacts', context.artifacts, '--output-mode'], '--output-mode requires a value'],
    [['--input', input, '--artifacts', context.artifacts, '--mode', 'text'], 'unknown argument --mode'],
    [['--input', input, '--artifacts', context.artifacts, '--output-mode', 'tiff'], '--output-mode must be text or layout'],
  ]
  for (const [args, mention] of cases) {
    const result = run(context, args, {})
    const error = refused(result, 'invalid_input')
    assert.ok(error.message.includes(mention), `${args.join(' ')}: ${error.message}`)
    assert.deepEqual(result.calls, [], args.join(' '))
    assert.deepEqual(artifactsOf(context), [], args.join(' '))
  }
})

test('the helper refuses a bad input file', t => {
  const context = makeContext(t)
  const directory = path.join(context.root, 'inputs')
  mkdirSync(directory)
  const cases = [
    [path.join(context.root, 'missing.png'), 'The input file is missing'],
    [directory, 'The input is not a regular file'],
    [writeFile(context.root, 'empty.png', Buffer.alloc(0)), 'The input file is empty'],
    [writeFile(context.root, 'notes.txt', 'not an image or a PDF'), 'The input is not a PNG, JPEG or PDF'],
  ]
  for (const [input, mention] of cases) {
    const result = run(context, ['--input', input, '--artifacts', context.artifacts], {})
    const error = refused(result, 'invalid_input')
    assert.ok(error.message.includes(mention), `${input}: ${error.message}`)
    assert.deepEqual(artifactsOf(context), [])
  }
  const sparse = writeFile(context.root, 'large.pdf', PDF)
  truncateSync(sparse, 101 * 1024 * 1024)
  const result = run(context, ['--input', sparse, '--artifacts', context.artifacts], {})
  refused(result, 'resource_limit_exceeded')
  assert.deepEqual(result.calls, [])
  assert.deepEqual(artifactsOf(context), [])
})

test('an image becomes one page, in text and layout mode', t => {
  const context = makeContext(t)
  const input = writeFile(context.root, 'photo.png', PNG)
  const settings = { recognizeEncoded: { imageWidth: 640, imageHeight: 480, lines: LINES } }
  for (const mode of ['text', 'layout']) {
    const result = run(context, ['--input', input, '--artifacts', context.artifacts, '--output-mode', mode], settings)
    assert.deepEqual(succeeded(result), { ok: true, pageCount: 1 })
    assert.deepEqual(result.calls.map(call => call.call),
      ['createEngine', 'recognizeEncoded', 'createEngine.close'], mode)
    const engine = result.calls[0].options
    assert.deepEqual(engine.execution, { provider: 'cpu' }, mode)
    assert.deepEqual(Object.keys(engine.reducedLimits).sort(), [
      'maxDetectionCandidates', 'maxDetectionSide', 'maxHeight', 'maxPixels',
      'maxRecognitionBatchSize', 'maxRecognitionWidth', 'maxTemporaryBytes', 'maxWidth',
    ], mode)
    assert.equal(engine.reducedLimits.maxPixels, PAGE_PIXEL_BOUND, mode)
    assert.deepEqual(result.calls[1],
      { call: 'recognizeEncoded', length: PNG.length, options: { applyExif: true } }, mode)
    assert.deepEqual(artifactsOf(context), ['result.json', 'text.txt'])
    assert.deepEqual(document(context), {
      text: 'DIMCODE\nOCR\nlow',
      pageCount: 1,
      pages: [{ page: 1, text: 'DIMCODE\nOCR\nlow', width: 640, height: 480, regions: REGIONS }],
    })
    assert.equal(readFileSync(path.join(context.artifacts, 'text.txt'), 'utf8'), 'DIMCODE\nOCR\nlow')
  }
})

test('an image at the page pixel bound is accepted and one pixel past it is refused', t => {
  const context = makeContext(t)
  const input = writeFile(context.root, 'photo.png', PNG)
  const refusedResult = run(context, ['--input', input, '--artifacts', context.artifacts], {
    recognizeEncoded: { imageWidth: PAGE_PIXEL_BOUND + 1, imageHeight: 1 },
  })
  const error = refused(refusedResult, 'resource_limit_exceeded')
  assert.ok(error.message.includes('Decoded image dimensions exceed engine limits'), error.message)
  assert.deepEqual(artifactsOf(context), [])
  assert.deepEqual(refusedResult.calls.map(call => call.call),
    ['createEngine', 'recognizeEncoded', 'createEngine.close'])

  const accepted = run(context, ['--input', input, '--artifacts', context.artifacts], {
    recognizeEncoded: { imageWidth: 5000, imageHeight: 5000, lines: [] },
  })
  assert.deepEqual(succeeded(accepted), { ok: true, pageCount: 1 })
  assert.deepEqual(artifactsOf(context), ['result.json', 'text.txt'])
  assert.deepEqual(document(context), {
    text: '',
    pageCount: 1,
    pages: [{ page: 1, text: '', width: 5000, height: 5000, regions: [] }],
  })
})

test('a PDF in text mode never resolves a page renderer', t => {
  const context = makeContext(t)
  const input = writeFile(context.root, 'document.pdf', PDF)
  const result = run(context, ['--input', input, '--artifacts', context.artifacts], { pages: PAGES })
  assert.deepEqual(succeeded(result), { ok: true, pageCount: 2 })
  assert.deepEqual(result.calls, [
    { call: 'hasPdfSupport' },
    { call: 'createDocumentEngine', options: { engineOptions: { execution: { provider: 'cpu' } } } },
    {
      call: 'recognizeDocument',
      length: PDF.length,
      options: { dpi: 150, maxPages: 500, maxPagePixels: 25000000, maxTotalPixels: 12500000000, maxFileBytes: 104857600 },
    },
    { call: 'createDocumentEngine.close' },
  ])
  assert.deepEqual(artifactsOf(context), ['result.json', 'text.txt'])
  assert.deepEqual(document(context), {
    text: 'page one\npage two',
    pageCount: 2,
    pages: [
      { page: 1, text: 'page one', width: 1000, height: 1400, regions: [{ text: 'page one', confidence: 0.9, polygon: PAGE_REGION }] },
      { page: 2, text: 'page two', width: 1000, height: 1400, regions: [{ text: 'page two', confidence: 0.8, polygon: PAGE_REGION }] },
    ],
  })
  assert.equal(readFileSync(path.join(context.artifacts, 'text.txt'), 'utf8'), 'page one\npage two')
})

test('a PDF in layout mode renders every page through its platform package', t => {
  const context = makeContext(t, { platform: true })
  const input = writeFile(context.root, 'document.pdf', PDF)
  const result = run(context, layoutArguments(context, input), {
    platform: { id: 'macos-arm64' },
    pages: PAGES,
    pdfium: { render: [{ bytes: 24 }, { bytes: 32 }] },
  })
  assert.deepEqual(succeeded(result), { ok: true, pageCount: 2 })
  assert.deepEqual(artifactsOf(context), ['page-0001.png', 'page-0002.png', 'result.json', 'text.txt'])
  const rendered = document(context)
  assert.equal(rendered.pages[0].imagePath, 'page-0001.png')
  assert.equal(rendered.pages[1].imagePath, 'page-0002.png')
  assert.equal(statSync(path.join(context.artifacts, 'page-0001.png')).size, 24)
  assert.equal(statSync(path.join(context.artifacts, 'page-0002.png')).size, 32)
  const pdfium = result.calls.filter(call => call.call.startsWith('pdfium.'))
  assert.deepEqual(pdfium[0], { call: 'pdfium.loadDocument', package: '@arcships/light-ocr-darwin-arm64', length: PDF.length })
  assert.deepEqual(pdfium.filter(call => call.call === 'pdfium.render'), [
    { call: 'pdfium.render', package: '@arcships/light-ocr-darwin-arm64', index: 0, options: { scale: 150 / 72 } },
    { call: 'pdfium.render', package: '@arcships/light-ocr-darwin-arm64', index: 1, options: { scale: 150 / 72 } },
  ])
  assert.deepEqual(pdfium.filter(call => call.call === 'pdfium.page.close'), [
    { call: 'pdfium.page.close', package: '@arcships/light-ocr-darwin-arm64', index: 0 },
    { call: 'pdfium.page.close', package: '@arcships/light-ocr-darwin-arm64', index: 1 },
  ])
  assert.deepEqual(pdfium.filter(call => call.call === 'pdfium.destroy'), [
    { call: 'pdfium.destroy', package: '@arcships/light-ocr-darwin-arm64' },
  ])
})

test('each platform package the lock installs loads for its own identity', t => {
  const context = makeContext(t, { platform: true })
  const input = writeFile(context.root, 'document.pdf', PDF)
  assert.deepEqual(platformPackages.map(({ identity }) => identity).sort(), [
    'linux-arm64', 'linux-arm64-musl', 'linux-x64', 'linux-x64-musl',
    'macos-arm64', 'macos-x64', 'windows-arm64', 'windows-x64',
  ], 'the lock no longer installs a package for every platform the helper names')
  for (const { identity, name } of platformPackages) {
    const result = run(context, layoutArguments(context, input), {
      platform: { id: identity },
      pages: [PAGES[0]],
      pdfium: { render: [{ bytes: 8 }] },
    })
    succeeded(result)
    const load = result.calls.find(call => call.call === 'pdfium.loadDocument')
    assert.equal(load?.package, name, `${identity} must load ${name}`)
  }
})

test('an unusable platform identity is a missing local runtime', t => {
  const context = makeContext(t, { platform: true })
  const input = writeFile(context.root, 'document.pdf', PDF)
  const unknown = run(context, layoutArguments(context, input), {
    platform: { id: 'darwin-x64-unknown' },
    pages: [PAGES[0]],
  })
  const unknownError = refused(unknown, 'local_runtime_unavailable')
  assert.ok(unknownError.message.includes('darwin-x64-unknown'), unknownError.message)
  assert.deepEqual(unknown.calls.filter(call => call.call.startsWith('pdfium.')), [])
  const failed = run(context, layoutArguments(context, input), {
    platform: { throw: 'light-ocr does not support darwin-x64' },
    pages: [PAGES[0]],
  })
  refused(failed, 'local_runtime_unavailable')
  assert.deepEqual(failed.calls.filter(call => call.call.startsWith('pdfium.')), [])
})

test('an engine error code becomes its helper code and an unknown one fails the runtime', t => {
  const context = makeContext(t)
  const input = writeFile(context.root, 'photo.png', PNG)
  const cases = [
    ['invalid_argument', 'invalid_input'],
    ['queue_full', 'resource_limit_exceeded'],
    ['model_integrity_failed', 'local_runtime_unavailable'],
    ['inference_failed', 'local_runtime_failed'],
    ['some_future_code', 'local_runtime_failed'],
  ]
  for (const [code, helperCode] of cases) {
    const result = run(context, ['--input', input, '--artifacts', context.artifacts], {
      recognizeEncoded: { throw: { code, message: `${code} from the fixture` } },
    })
    const error = refused(result, helperCode)
    assert.equal(error.message, `${code} from the fixture`, code)
    assert.deepEqual(artifactsOf(context), [], code)
  }
})

test('a text-mode document load failure is classified by its message', t => {
  const context = makeContext(t)
  const input = writeFile(context.root, 'document.pdf', PDF)
  const cases = [
    ['PASSWORD: password required', 'encrypted_pdf'],
    ['FORMAT: not a PDF', 'invalid_input'],
  ]
  for (const [text, code] of cases) {
    const result = run(context, ['--input', input, '--artifacts', context.artifacts], {
      recognizeDocument: { throw: { message: text } },
    })
    refused(result, code)
    assert.deepEqual(artifactsOf(context), [], text)
  }
})

test('a page renderer load is classified by message and a second render fails the runtime', t => {
  const context = makeContext(t, { platform: true })
  const input = writeFile(context.root, 'document.pdf', PDF)
  const cases = [
    [{ load: { throw: 'PASSWORD: password required' } }, 'encrypted_pdf'],
    [{ load: { throw: 'SECURITY: cipher not supported' } }, 'encrypted_pdf'],
    [{ load: { throw: 'FORMAT: not a PDF' } }, 'invalid_input'],
    [{ page: { throw: 'PAGE: no such page' } }, 'local_runtime_failed'],
    [{ render: [{ throw: 'UNKNOWN: the renderer crashed' }] }, 'local_runtime_failed'],
    [{ load: { throw: 'boom' } }, 'local_runtime_failed'],
  ]
  for (const [pdfium, code] of cases) {
    const result = run(context, layoutArguments(context, input), {
      platform: { id: 'macos-arm64' },
      pages: [PAGES[0]],
      pdfium,
    })
    refused(result, code)
    assert.deepEqual(artifactsOf(context), [], JSON.stringify(pdfium))
  }
})

test('a plain engine failure fails the local runtime', t => {
  const context = makeContext(t)
  const input = writeFile(context.root, 'photo.jpg', JPEG)
  const result = run(context, ['--input', input, '--artifacts', context.artifacts], {
    recognizeEncoded: { throw: { message: 'boom' } },
  })
  const error = refused(result, 'local_runtime_failed')
  assert.equal(error.message, 'boom')
  assert.deepEqual(artifactsOf(context), [])
})

test('pdf support false refuses both modes before the engine', t => {
  const context = makeContext(t, { platform: true })
  const input = writeFile(context.root, 'document.pdf', PDF)
  const settings = {
    hasPdfSupport: false,
    platform: { id: 'macos-arm64' },
    pages: [PAGES[0]],
    pdfium: { render: [{ bytes: 8 }] },
  }
  for (const mode of ['text', 'layout']) {
    const result = run(context, ['--input', input, '--artifacts', context.artifacts, '--output-mode', mode], settings)
    const error = refused(result, 'local_runtime_unavailable')
    assert.ok(error.message.includes('PDF support'), `${mode}: ${error.message}`)
    assert.deepEqual(result.calls, [{ call: 'hasPdfSupport' }], mode)
    assert.deepEqual(artifactsOf(context), [], mode)
  }
})

test('the artifact budget refuses the page that crosses it', t => {
  const context = makeContext(t, { platform: true })
  const input = writeFile(context.root, 'document.pdf', PDF)
  const result = run(context, layoutArguments(context, input), {
    platform: { id: 'macos-arm64' },
    pages: PAGES,
    pdfium: { render: [{ bytes: 24 }, { bytes: 1 << 30 }] },
  })
  refused(result, 'resource_limit_exceeded')
  assert.deepEqual(artifactsOf(context), ['page-0001.png'])
  assert.equal(statSync(path.join(context.artifacts, 'page-0001.png')).size, 24)
  assert.ok(result.calls.some(call => call.call === 'pdfium.page.close' && call.index === 1))
  assert.ok(result.calls.some(call => call.call === 'pdfium.destroy'))
})

test('the helper writes nothing outside the artifact directory', t => {
  const context = makeContext(t, { platform: true })
  const before = listing(context.plugin)
  const image = writeFile(context.root, 'photo.png', PNG)
  succeeded(run(context, ['--input', image, '--artifacts', context.artifacts], {
    recognizeEncoded: { imageWidth: 64, imageHeight: 48, lines: LINES },
  }))
  const pdf = writeFile(context.root, 'document.pdf', PDF)
  succeeded(run(context, layoutArguments(context, pdf), {
    platform: { id: 'macos-arm64' },
    pages: PAGES,
    pdfium: { render: [{ bytes: 8 }, { bytes: 8 }] },
  }))
  assert.deepEqual(listing(context.plugin), before)
})

test('an unloadable facade is a missing local runtime', t => {
  const context = makeContext(t)
  const input = writeFile(context.root, 'photo.png', PNG)
  const result = run(context, ['--input', input, '--artifacts', context.artifacts], {
    require: { throw: 'Cannot find module @arcships/light-ocr' },
  })
  const error = refused(result, 'local_runtime_unavailable')
  assert.ok(error.message.includes('Cannot find module'), error.message)
  assert.deepEqual(result.calls, [])
  assert.deepEqual(artifactsOf(context), [])
})

test('an artifact path that is not a directory is invalid input', t => {
  const context = makeContext(t)
  const input = writeFile(context.root, 'photo.png', PNG)
  const file = writeFile(context.root, 'artifacts-file', 'not a directory')
  for (const artifacts of [file, path.join(context.root, 'missing')]) {
    const result = run(context, ['--input', input, '--artifacts', artifacts], {})
    const error = refused(result, 'invalid_input')
    assert.ok(error.message.includes('The artifact directory is missing'), error.message)
  }
})

test('the plugin manifest and its lock agree', () => {
  const directory = path.join(pluginSource)
  const manifest = JSON.parse(readFileSync(path.join(directory, 'package.json'), 'utf8'))
  const lock = JSON.parse(readFileSync(path.join(directory, 'package-lock.json'), 'utf8'))
  assert.equal(lock.lockfileVersion, 3)
  assert.deepEqual(Object.keys(manifest.dependencies).sort(), [
    '@arcships/light-ocr',
    '@arcships/light-ocr-runtime',
    '@arcships/rutis',
  ])
  assert.deepEqual(lock.packages[''].dependencies, manifest.dependencies)
  for (const [name, entry] of Object.entries(lock.packages)) {
    if (name === '') continue
    assert.ok(name.startsWith('node_modules/'), name)
    assert.equal(typeof entry.integrity, 'string', name)
    assert.equal(entry.hasInstallScript, undefined, name)
  }
  const facade = lock.packages['node_modules/@arcships/light-ocr']
  const runtime = lock.packages['node_modules/@arcships/light-ocr-runtime']
  assert.equal(facade.dependencies['@arcships/light-ocr-runtime'], runtime.version)
  assert.equal(manifest.dependencies['@arcships/light-ocr-runtime'], runtime.version)
  assert.equal(lock.packages['node_modules/@arcships/rutis'].version, manifest.dependencies['@arcships/rutis'])
})
