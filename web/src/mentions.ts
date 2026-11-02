/**
 * `@` file mentions in the composer: the trigger before the caret, and how a
 * chosen file reads when the message is sent. A port of the TUI's trigger
 * (`crates/dim-cli/src/ui/composer/mentions.rs`) and file link
 * (`crates/dim-cli/src/attachments.rs`, `file_link`).
 */

export type MentionTrigger = { start: number; query: string }

/** A file the composer inserted: the `@path` text it shows and the absolute path it sends. */
export type FileMention = { label: string; path: string }

const BOUNDARY = /[\s,;:!?，。！？、()[\]{}<>]/
const QUERY_CHAR = /[\p{L}\p{N}_./~\\:-]/u

/** The `@` the caret is in: an `@` at the start or after a boundary, then path characters up to the caret. */
export function mentionTrigger(text: string, cursor: number): MentionTrigger | undefined {
  const before = text.slice(0, cursor)
  for (let index = before.length - 1; index >= 0; index--) {
    const ch = before[index]!
    if (ch === "@") {
      const lead = index === 0 ? undefined : before[index - 1]!
      if (lead !== undefined && !BOUNDARY.test(lead)) return undefined
      return { start: index, query: before.slice(index + 1) }
    }
    if (!QUERY_CHAR.test(ch)) return undefined
  }
  return undefined
}

/** A Markdown link to a local file: `[label](path)`, the path in angle brackets when it has spaces or parentheses. */
export function fileLink(label: string, path: string): string {
  const text = label.replace(/[[\]\\]/g, (ch) => `\\${ch}`)
  if (/[\s()<>]/.test(path)) return `[${text}](<${path.replaceAll("<", "%3C").replaceAll(">", "%3E")}>)`
  return `[${text}](${path})`
}

/**
 * The message with each inserted file's `@path` replaced by its link. A label
 * counts only where the text still has it as a whole word, so a file whose
 * name is a prefix of another's is not linked by mistake.
 */
export function linkMentions(text: string, mentions: readonly FileMention[]): string {
  const spans: Array<{ start: number; end: number; link: string }> = []
  for (const mention of mentions) {
    let from = 0
    for (;;) {
      const start = text.indexOf(mention.label, from)
      if (start === -1) break
      const end = start + mention.label.length
      const next = text[end]
      if (next === undefined || !QUERY_CHAR.test(next)) spans.push({ start, end, link: fileLink(mention.label, mention.path) })
      from = end
    }
  }
  spans.sort((left, right) => left.start - right.start || right.end - left.end)
  let out = ""
  let cursor = 0
  for (const span of spans) {
    if (span.start < cursor) continue
    out += text.slice(cursor, span.start) + span.link
    cursor = span.end
  }
  return out + text.slice(cursor)
}
