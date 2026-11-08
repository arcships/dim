'use strict'

const fs = require('node:fs')
const path = require('node:path')

const MAX_INPUT_BYTES = 100 * 1024 * 1024
const MAX_ARTIFACT_BYTES = 1024 * 1024 * 1024
const MAX_PAGES = 500
const MAX_PAGE_PIXELS = 25_000_000
const MAX_TOTAL_PIXELS = 12_500_000_000
const MAX_FILE_BYTES = 104_857_600
const DPI = 150
const MAX_MESSAGE = 4096

/** The limits the image engine carries: the pinned model bundle's own
 * resource limits (`@arcships/light-ocr-model-ppocrv6-small` 0.3.4,
 * `bundle/normalized-config.json`) with Modality's page bound as `maxPixels`.
 * An engine may only reduce its bundle's limits, and `reducedLimits` must
 * carry every legacy field, so the untouched limits stay the bundle's. */
const IMAGE_LIMITS = {
  maxWidth: 10_000,
  maxHeight: 10_000,
  maxPixels: MAX_PAGE_PIXELS,
  maxDetectionSide: 4_000,
  maxDetectionCandidates: 3_000,
  maxRecognitionBatchSize: 8,
  maxRecognitionWidth: 3_200,
  maxTemporaryBytes: 512 * 1024 * 1024,
}

/** The platform package of each light-ocr platform identity, as the facade's
 * own document API names them. */
const PLATFORM_PACKAGES = {
  'macos-arm64': '@arcships/light-ocr-darwin-arm64',
  'macos-x64': '@arcships/light-ocr-darwin-x64',
  'windows-arm64': '@arcships/light-ocr-win32-arm64',
  'windows-x64': '@arcships/light-ocr-win32-x64',
  'linux-x64': '@arcships/light-ocr-linux-x64-gnu',
  'linux-arm64': '@arcships/light-ocr-linux-arm64-gnu',
  'linux-x64-musl': '@arcships/light-ocr-linux-x64-musl',
  'linux-arm64-musl': '@arcships/light-ocr-linux-arm64-musl',
}

/** Each light-ocr error code as the helper contract's code. */
const ERROR_CODES = {
  invalid_argument: 'invalid_input',
  invalid_image: 'invalid_input',
  unsupported_pixel_format: 'invalid_input',
  unsupported_capability: 'invalid_input',
  resource_limit_exceeded: 'resource_limit_exceeded',
  queue_full: 'resource_limit_exceeded',
  invalid_model_bundle: 'local_runtime_unavailable',
  unsupported_model: 'local_runtime_unavailable',
  model_integrity_failed: 'local_runtime_unavailable',
  runtime_initialization_failed: 'local_runtime_unavailable',
  invalid_engine: 'local_runtime_unavailable',
  bundle_io_failed: 'local_runtime_unavailable',
  unsupported_platform: 'local_runtime_unavailable',
  package_load_failed: 'local_runtime_unavailable',
  environment_closing: 'local_runtime_unavailable',
  internal_error: 'local_runtime_unavailable',
  inference_failed: 'local_runtime_failed',
  postprocess_failed: 'local_runtime_failed',
}

class Refusal extends Error {
  constructor(code, message) {
    super(message)
    this.code = code
  }
}

function refuse(code, message) {
  throw new Refusal(code, message)
}

function message(error) {
  const text = error instanceof Error ? error.message : String(error)
  return text.length > MAX_MESSAGE ? text.slice(0, MAX_MESSAGE) : text
}

let facade
function lightOcr() {
  if (facade === undefined) {
    try {
      facade = require('@arcships/light-ocr')
    } catch (error) {
      refuse('local_runtime_unavailable', `The light-ocr package cannot be loaded: ${message(error)}`)
    }
  }
  return facade
}

/** The helper code one engine failure maps to: an `OcrError` by its code, a
 * PDF renderer failure by PDFium's message prefix, anything else as a failed
 * local runtime. */
function classify(error) {
  if (facade !== undefined && error instanceof facade.OcrError) {
    return ERROR_CODES[error.code] ?? 'local_runtime_failed'
  }
  const text = message(error)
  if (/^(PASSWORD|SECURITY):/.test(text)) return 'encrypted_pdf'
  if (/^(FILE|FORMAT|PAGE|UNKNOWN):/.test(text)) return 'invalid_input'
  return 'local_runtime_failed'
}

function parse(argv) {
  const flags = { '--input': 'input', '--artifacts': 'artifacts', '--output-mode': 'outputMode' }
  const options = { input: undefined, artifacts: undefined, outputMode: 'text' }
  for (let index = 0; index < argv.length; index += 2) {
    const argument = argv[index]
    if (flags[argument] === undefined) refuse('invalid_input', `unknown argument ${argument}`)
    const value = argv[index + 1]
    if (value === undefined) refuse('invalid_input', `${argument} requires a value`)
    options[flags[argument]] = value
  }
  if (options.input === undefined) refuse('invalid_input', '--input is required')
  if (options.artifacts === undefined) refuse('invalid_input', '--artifacts is required')
  if (options.outputMode !== 'text' && options.outputMode !== 'layout') {
    refuse('invalid_input', '--output-mode must be text or layout')
  }
  return options
}

function artifactsDirectory(directory) {
  const metadata = fs.statSync(directory, { throwIfNoEntry: false })
  if (metadata === undefined || !metadata.isDirectory()) {
    refuse('invalid_input', 'The artifact directory is missing')
  }
  return directory
}

function readInput(file) {
  const metadata = fs.statSync(file, { throwIfNoEntry: false })
  if (metadata === undefined) refuse('invalid_input', 'The input file is missing')
  if (!metadata.isFile()) refuse('invalid_input', 'The input is not a regular file')
  if (metadata.size === 0) refuse('invalid_input', 'The input file is empty')
  if (metadata.size > MAX_INPUT_BYTES) {
    refuse('resource_limit_exceeded', `The input exceeds ${MAX_INPUT_BYTES} bytes`)
  }
  return fs.readFileSync(file)
}

function sniff(bytes) {
  if (bytes.length >= 4 && bytes[0] === 0x25 && bytes[1] === 0x50 && bytes[2] === 0x44 && bytes[3] === 0x46) {
    return 'pdf'
  }
  if (bytes.length >= 4 && bytes[0] === 0x89 && bytes[1] === 0x50 && bytes[2] === 0x4e && bytes[3] === 0x47) {
    return 'image'
  }
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
    return 'image'
  }
  return undefined
}

class Artifacts {
  constructor(directory) {
    this.directory = directory
    this.bytes = 0
  }

  write(name, data) {
    const next = this.bytes + data.length
    if (next > MAX_ARTIFACT_BYTES) {
      refuse('resource_limit_exceeded', `The OCR artifacts exceed ${MAX_ARTIFACT_BYTES} bytes`)
    }
    fs.writeFileSync(path.join(this.directory, name), data)
    this.bytes = next
    return name
  }
}

function pageImage(number) {
  return `page-${String(number).padStart(4, '0')}.png`
}

function page(number, width, height, lines) {
  return {
    page: number,
    text: lines.map(line => line.text).join('\n'),
    width,
    height,
    regions: lines.map(line => ({
      text: line.text,
      confidence: Math.min(1, Math.max(0, line.confidence)),
      polygon: line.box.map(point => [point.x, point.y]),
    })),
  }
}

async function images(api, bytes) {
  const engine = await api.createEngine({ execution: { provider: 'cpu' }, reducedLimits: IMAGE_LIMITS })
  try {
    const result = await engine.recognizeEncoded(bytes, { applyExif: true })
    return [page(1, result.imageWidth, result.imageHeight, result.lines)]
  } finally {
    await engine.close()
  }
}

function platformPdfium() {
  let runtime
  try {
    runtime = require('@arcships/light-ocr-runtime')
  } catch (error) {
    refuse('local_runtime_unavailable', `The light-ocr runtime package cannot be loaded: ${message(error)}`)
  }
  let id
  try {
    id = runtime.platformIdentity().id
  } catch (error) {
    refuse('local_runtime_unavailable', message(error))
  }
  const name = PLATFORM_PACKAGES[id]
  if (name === undefined) refuse('local_runtime_unavailable', `No light-ocr platform package for ${id}`)
  try {
    return require(`${name}/pdfium`)
  } catch (error) {
    refuse('local_runtime_unavailable', `The PDF renderer cannot be loaded: ${message(error)}`)
  }
}

/** The second render of a page the engine has already loaded, rendered and
 * recognized: whatever fails here and whatever its message, the local runtime
 * failed, not the input. */
async function render(document, index) {
  try {
    const rendered = await document.getPage(index)
    try {
      return await rendered.render({ scale: DPI / 72 })
    } finally {
      await rendered.close()
    }
  } catch (error) {
    refuse('local_runtime_failed', message(error))
  }
}

async function pdf(api, bytes, mode, artifacts) {
  if (!api.hasPdfSupport()) {
    refuse('local_runtime_unavailable', 'PDF support is unavailable in the installed light-ocr packages')
  }
  const renderer = mode === 'layout' ? platformPdfium() : undefined
  const document = renderer === undefined ? undefined : await renderer.loadDocument(bytes)
  try {
    const engine = await api.createDocumentEngine({ engineOptions: { execution: { provider: 'cpu' } } })
    try {
      const pages = []
      for await (const recognized of engine.recognizeDocument(bytes, {
        dpi: DPI,
        maxPages: MAX_PAGES,
        maxPagePixels: MAX_PAGE_PIXELS,
        maxTotalPixels: MAX_TOTAL_PIXELS,
        maxFileBytes: MAX_FILE_BYTES,
      })) {
        const number = recognized.index + 1
        const entry = page(number, recognized.width, recognized.height, recognized.lines)
        if (document !== undefined) {
          entry.imagePath = artifacts.write(pageImage(number), await render(document, recognized.index))
        }
        pages.push(entry)
      }
      return pages
    } finally {
      await engine.close()
    }
  } finally {
    if (document !== undefined) await document.destroy()
  }
}

async function main() {
  try {
    const options = parse(process.argv.slice(2))
    const artifacts = new Artifacts(artifactsDirectory(options.artifacts))
    const bytes = readInput(options.input)
    const kind = sniff(bytes)
    if (kind === undefined) refuse('invalid_input', 'The input is not a PNG, JPEG or PDF')
    const api = lightOcr()
    const pages = kind === 'pdf'
      ? await pdf(api, bytes, options.outputMode, artifacts)
      : await images(api, bytes)
    const text = pages.map(entry => entry.text).join('\n')
    artifacts.write('result.json', Buffer.from(JSON.stringify({ text, pageCount: pages.length, pages })))
    artifacts.write('text.txt', Buffer.from(text))
    process.stdout.write(`${JSON.stringify({ ok: true, pageCount: pages.length })}\n`)
    return 0
  } catch (error) {
    const code = error instanceof Refusal ? error.code : classify(error)
    process.stdout.write(`${JSON.stringify({ ok: false, error: { code, message: message(error) } })}\n`)
    return 2
  }
}

if (require.main === module) {
  main().then(code => {
    process.exitCode = code
  })
}
