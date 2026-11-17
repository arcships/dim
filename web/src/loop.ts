/** What `/loop` does with the words after it. */
export type LoopRequest =
  | { form: "show" }
  | { form: "start"; prompt: string; interval?: string }
  | { form: "stop"; loopId?: string }

/** An interval token: a number and one of `s`, `m`, `h`, `d`. */
export function parseInterval(token: string): string | undefined {
  const match = /^([0-9]+)([smhd])$/.exec(token)
  if (match === null) return undefined
  return Number(match[1]) > 0 ? token : undefined
}

/**
 * Reads `/loop`'s words the way the TUI does (cli-commands.md): the interval
 * counts only as the first token, and only when it parses; otherwise the whole
 * line is the prompt. `undefined` is a usage error.
 */
export function parseLoopRequest(words: string): LoopRequest | undefined {
  const trimmed = words.trim()
  if (trimmed === "") return { form: "show" }

  if (trimmed === "stop") return { form: "stop" }
  if (trimmed.startsWith("stop ") || trimmed === "stop") {
    const rest = trimmed.slice("stop ".length).trim()
    if (rest === "") return { form: "stop" }
    if (/\s/.test(rest)) return undefined
    return { form: "stop", loopId: rest }
  }

  const space = trimmed.search(/\s/)
  if (space !== -1) {
    const first = trimmed.slice(0, space)
    const rest = trimmed.slice(space).trim()
    const interval = parseInterval(first)
    if (interval !== undefined && rest !== "") {
      return { form: "start", prompt: rest, interval }
    }
  }
  return { form: "start", prompt: trimmed }
}

/** One loop as `session.loop.list` reports it. */
export type Loop = {
  loopId: string
  state: string
  round: number
  nextAt: string | null
  prompt: string
}

/** Reads one loop from the wire; an entry without an id is not a loop. */
export function readLoop(value: unknown): Loop | undefined {
  if (typeof value !== "object" || value === null) return undefined
  const record = value as Record<string, unknown>
  const loopId = record.loopId
  if (typeof loopId !== "string" || loopId === "") return undefined
  return {
    loopId,
    state: typeof record.state === "string" ? record.state : "unknown",
    round: typeof record.round === "number" ? record.round : 0,
    nextAt: typeof record.nextAt === "string" ? record.nextAt : null,
    prompt: typeof record.prompt === "string" ? record.prompt : "",
  }
}

/** The lines `/loop` prints, the same words the terminal client uses. */
export function loopReport(loops: readonly Loop[]): readonly string[] {
  if (loops.length === 0) return ["No loop on this line."]
  return loops.map((loop) => `${loop.loopId} ${loop.state} round ${loop.round}${loop.nextAt === null ? "" : ` next ${loop.nextAt}`} ${loop.prompt}`)
}

/** The interval in whole seconds, rounded up to whole minutes at the server. */
export function intervalSeconds(token: string): number | undefined {
  const match = /^([0-9]+)([smhd])$/.exec(token)
  if (match === null) return undefined
  const value = Number(match[1])
  if (value === 0) return undefined
  return value * { s: 1, m: 60, h: 3600, d: 86400 }[match[2] as "s" | "m" | "h" | "d"]
}

/** The `loopId` a `session.loop.start` reply names. */
export function readStartedLoop(value: unknown): string {
  if (typeof value !== "object" || value === null) return ""
  const loop = (value as Record<string, unknown>).loop
  const id = readLoop(loop)?.loopId ?? ""
  return id
}

/** The loops a `session.loop.list` reply carries. */
export function readLoops(value: unknown): readonly Loop[] {
  if (typeof value !== "object" || value === null) return []
  const list = (value as Record<string, unknown>).loops
  if (!Array.isArray(list)) return []
  return list.flatMap((entry) => {
    const loop = readLoop(entry)
    return loop === undefined ? [] : [loop]
  })
}
