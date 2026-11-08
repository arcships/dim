'use strict'

// A stand-in for `@arcships/light-ocr-runtime` 0.1.9: the one export the
// PP-OCR helper uses, `platformIdentity`, which names whatever platform the
// plan does.

const fs = require('node:fs')

function plan() {
  const file = process.env.LIGHT_OCR_FIXTURE_PLAN
  if (file === undefined) throw new Error('The light-ocr fixture needs LIGHT_OCR_FIXTURE_PLAN')
  return JSON.parse(fs.readFileSync(file, 'utf8'))
}

exports.platformIdentity = () => {
  const platform = plan().platform ?? {}
  if (platform.throw !== undefined) throw new Error(platform.throw)
  return { id: platform.id ?? 'macos-arm64', os: 'fixture', architecture: 'fixture' }
}
