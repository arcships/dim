'use strict'

// A stand-in for a light-ocr platform package's `./pdfium` export: the
// document and page API exactly as the PP-OCR helper uses it. The test copies
// this file into each of the eight package names, so the package it was
// loaded from names itself in the log: a wrong entry in the helper's table
// loads a different package and is visible there.

const fs = require('node:fs')
const path = require('node:path')

const name = require(path.join(__dirname, '..', 'package.json')).name

function plan() {
  const file = process.env.LIGHT_OCR_FIXTURE_PLAN
  if (file === undefined) throw new Error('The light-ocr fixture needs LIGHT_OCR_FIXTURE_PLAN')
  return JSON.parse(fs.readFileSync(file, 'utf8'))
}

function log(entry) {
  const file = process.env.LIGHT_OCR_FIXTURE_LOG
  if (file !== undefined) fs.appendFileSync(file, `${JSON.stringify(entry)}\n`)
}

// A `{throw}` throws its message as a plain Error, as PDFium's addon does.
function fail(spec) {
  if (spec?.throw !== undefined) throw new Error(spec.throw)
}

exports.loadDocument = async function loadDocument(bytes) {
  log({ call: 'pdfium.loadDocument', package: name, length: bytes.length })
  const settings = plan().pdfium ?? {}
  fail(settings.load)
  return {
    async getPage(index) {
      log({ call: 'pdfium.getPage', package: name, index })
      fail(settings.page)
      const rendered = Array.isArray(settings.render) ? (settings.render[index] ?? {}) : (settings.render ?? {})
      return {
        async render(options) {
          log({ call: 'pdfium.render', package: name, index, options })
          fail(rendered)
          return Buffer.allocUnsafe(rendered.bytes ?? 16)
        },
        async close() {
          log({ call: 'pdfium.page.close', package: name, index })
        },
      }
    },
    async destroy() {
      log({ call: 'pdfium.destroy', package: name })
    },
  }
}
