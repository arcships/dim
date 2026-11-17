/**
 * The `/skill:<name>` token, mirroring `crates/dim-agent-types/src/skill_token.rs`:
 * where a token may start, which characters a name holds, and the spans a text
 * carries. The composer narrows its menu with it and the transcript draws each
 * token as one unit, so both agree with the server on what loads.
 *
 * A token is `/skill:` and then one or more name characters, with no space
 * after the colon. It starts a text or follows whitespace, so it may stand
 * mid-sentence and never starts inside a word: `a/skill:x` and
 * `https://host/skill:y` carry none. A name never ends with a full stop, so a
 * sentence's final stop stays the operator's text: `/skill:review.` names
 * `review` and keeps its full stop.
 */

export const SKILL_TOKEN_PREFIX = "/skill:"

/** Whether `ch` may stand in a Skill name a token carries. */
export function isSkillNameChar(ch: string): boolean {
  return /^[A-Za-z0-9_.-]$/.test(ch)
}

/** Whether `name` is a Skill name a token can carry; one ending in a full stop cannot be. */
export function namesSkillToken(name: string): boolean {
  return name.length > 0 && !name.endsWith(".") && [...name].every(isSkillNameChar)
}

/** One token a text carries: the span it occupies and its name. */
export type SkillToken = { start: number; end: number; name: string }

/** The tokens `text` carries, in the order they appear. */
export function skillTokens(text: string): SkillToken[] {
  const found: SkillToken[] = []
  let from = 0
  for (;;) {
    const at = text.indexOf(SKILL_TOKEN_PREFIX, from)
    if (at === -1) break
    const after = at + SKILL_TOKEN_PREFIX.length
    let end = after
    while (end < text.length && isSkillNameChar(text[end]!)) end += 1
    // A full stop between other characters stays in the name; one that would
    // end it is the sentence's own and stays text.
    while (end > after && text[end - 1] === ".") end -= 1
    const leads = at === 0 || /\s/.test(text[at - 1]!)
    if (leads && end > after) {
      found.push({ start: at, end, name: text.slice(after, end) })
      from = end
    } else {
      from = after
    }
  }
  return found
}

/**
 * A draft split into what the composer draws: its words, with each
 * `/skill:<name>` token marked, so the input can draw the token as one chip
 * above the textarea that holds the same text.
 */
export function skillSegments(text: string): Array<{ text: string; skill: boolean }> {
  const segments: Array<{ text: string; skill: boolean }> = []
  let at = 0
  for (const token of skillTokens(text)) {
    if (token.start > at) segments.push({ text: text.slice(at, token.start), skill: false })
    segments.push({ text: text.slice(token.start, token.end), skill: true })
    at = token.end
  }
  if (at < text.length) segments.push({ text: text.slice(at), skill: false })
  return segments
}

/**
 * A draft that begins with `/skill:`, which is sent as a message and never run
 * as a command, so the composer does not treat it as a command prefix.
 */
export function startsWithSkillToken(text: string): boolean {
  return text.startsWith(SKILL_TOKEN_PREFIX)
}

/**
 * The Skill name a command-menu query narrows to: `/skill:de` asks for Skills
 * whose names start with `de`, and any other query asks for all of them.
 */
export function skillCommandQuery(text: string): { name: string; skillsOnly: boolean } {
  const rest = text.slice(1)
  const namespace = rest.startsWith("skill:") ? rest.slice("skill:".length) : undefined
  return namespace === undefined
    ? { name: rest.toLowerCase(), skillsOnly: false }
    : { name: namespace.toLowerCase(), skillsOnly: true }
}
