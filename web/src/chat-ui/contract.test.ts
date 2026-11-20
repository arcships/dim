import { readdirSync, readFileSync } from "node:fs"
import { join, relative, resolve } from "node:path"
import { describe, expect, it } from "vitest"
import { CHAT_UI_MESSAGE_KEYS } from "@dimcode/chat-ui"
import { chatUiMessagesEn, chatUiMessagesZhCN } from "./i18n"

const src = resolve(process.cwd(), "src")

function sources(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) return sources(path)
    return /\.(ts|vue|mjs)$/.test(entry.name) ? [path] : []
  })
}

describe("shared timeline contract", () => {
  it("translates every chatUi key and every host label in English and Chinese", () => {
    for (const key of CHAT_UI_MESSAGE_KEYS) {
      expect(chatUiMessagesEn[key], key).toBeTruthy()
      expect(chatUiMessagesZhCN[key], key).toBeTruthy()
    }
    expect(Object.keys(chatUiMessagesZhCN).sort()).toEqual(Object.keys(chatUiMessagesEn).sort())
  })

  it("imports @dimcode/chat-ui only under src/chat-ui", () => {
    const importers = sources(src)
      .filter(file => /from\s+["']@dimcode\/chat-ui|import\(\s*["']@dimcode\/chat-ui/.test(readFileSync(file, "utf8")))
      .map(file => relative(src, file).split("\\").join("/"))
    expect(importers.length).toBeGreaterThan(0)
    expect(importers.filter(file => !file.startsWith("chat-ui/"))).toEqual([])
  })
})
