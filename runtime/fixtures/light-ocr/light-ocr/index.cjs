'use strict'

// A stand-in for `@arcships/light-ocr` 0.5.9: the exports the PP-OCR helper
// uses, plus `getVersion`, which it does not. Every call is logged as one JSON
// object per line to `LIGHT_OCR_FIXTURE_LOG`; what each call returns or throws
// comes from `LIGHT_OCR_FIXTURE_PLAN`. Nothing here downloads, installs or
// loads the real engine.

const fs = require('node:fs')

class OcrError extends Error {
  constructor(code, message) {
    super(message)
    this.name = 'OcrError'
    this.code = code
  }
}

function plan() {
  const file = process.env.LIGHT_OCR_FIXTURE_PLAN
  if (file === undefined) throw new Error('The light-ocr fixture needs LIGHT_OCR_FIXTURE_PLAN')
  return JSON.parse(fs.readFileSync(file, 'utf8'))
}

function log(entry) {
  const file = process.env.LIGHT_OCR_FIXTURE_LOG
  if (file !== undefined) fs.appendFileSync(file, `${JSON.stringify(entry)}\n`)
}

// A `{code, message}` throws an `OcrError`; a bare `{message}`, a plain Error.
function fail(spec) {
  if (spec === undefined) return
  if (spec.code === undefined) throw new Error(spec.message)
  throw new OcrError(spec.code, spec.message)
}

const settings = plan()
if (settings.require !== undefined) throw new Error(settings.require.throw)

exports.createEngine = async function createEngine(options) {
  log({ call: 'createEngine', options })
  fail(settings.createEngine?.throw)
  const maxPixels = options?.reducedLimits?.maxPixels
  return {
    async recognizeEncoded(bytes, callOptions) {
      log({ call: 'recognizeEncoded', length: bytes.length, options: callOptions })
      fail(settings.recognizeEncoded?.throw)
      const spec = settings.recognizeEncoded ?? {}
      // The real engine checks the decoded size against the engine's
      // `maxPixels` while it decodes, before inference.
      if (typeof maxPixels === 'number' && spec.imageWidth * spec.imageHeight > maxPixels) {
        throw new OcrError('resource_limit_exceeded', 'Decoded image dimensions exceed engine limits')
      }
      return { imageWidth: spec.imageWidth, imageHeight: spec.imageHeight, lines: spec.lines ?? [] }
    },
    async close() {
      log({ call: 'createEngine.close' })
    },
  }
}

exports.createDocumentEngine = async function createDocumentEngine(options) {
  log({ call: 'createDocumentEngine', options })
  fail(settings.createDocumentEngine?.throw)
  return {
    async *recognizeDocument(bytes, callOptions) {
      log({ call: 'recognizeDocument', length: bytes.length, options: callOptions })
      fail(settings.recognizeDocument?.throw)
      for (const page of settings.pages ?? []) {
        fail(page.throw)
        yield { index: page.index, width: page.width, height: page.height, lines: page.lines ?? [] }
      }
    },
    async close() {
      log({ call: 'createDocumentEngine.close' })
    },
  }
}

exports.getVersion = () => '0.5.9'

exports.hasPdfSupport = () => {
  log({ call: 'hasPdfSupport' })
  return settings.hasPdfSupport ?? true
}

exports.OcrError = OcrError
