/**
 * The chat page's grouping of timeline rows: consecutive reasoning and tool
 * rows read as one work group ("Thought 2 times · 3 reads, 1 command") that
 * expands into its items; messages stand on their own.
 */
import type { TimelineRow } from "../../projection"
import type { ToolResult } from "../../rpc/protocol"
import { resolveToolDisplay } from "./tool-display"

/** What a work group's summary counts a tool call as. */
export type ToolCategory = "read" | "edit" | "command" | "search" | "web_search" | "web_fetch" | "unknown"

export type WorkItem =
  | { id: string; kind: "thinking"; content: string; running: boolean; durationMs?: number }
  | {
    id: string
    kind: "tool"
    name: string
    category: ToolCategory
    /** The collapsed row's inline summary: command, pattern, URL or file name. */
    primary?: string
    status: "running" | "complete" | "error"
    input: Record<string, unknown>
    result?: ToolResult
  }

export type DisplayRow =
  | { key: string; kind: "work"; items: WorkItem[] }
  | { key: string; kind: "row"; row: Exclude<TimelineRow, { kind: "thinking" | "tool" }> }

export function groupRows(rows: readonly TimelineRow[]): DisplayRow[] {
  const display: DisplayRow[] = []
  let work: WorkItem[] = []
  const flush = (): void => {
    if (work.length > 0) display.push({ key: `work-${work[0]!.id}`, kind: "work", items: work })
    work = []
  }
  for (const row of rows) {
    if (row.kind === "thinking") {
      // A settled thought that was never written (interrupted) shows nothing.
      if (row.text === "" && !row.streaming) continue
      work.push({ id: row.key, kind: "thinking", content: row.text, running: row.streaming, ...(row.elapsedMs === undefined ? {} : { durationMs: row.elapsedMs }) })
    } else if (row.kind === "tool") {
      const category = toolCategory(row.name, row.input)
      const primary = toolPrimary(category, row.input)
      work.push({
        id: row.key,
        kind: "tool",
        name: row.name,
        category,
        ...(primary === undefined ? {} : { primary }),
        status: row.pending ? "running" : row.result?.isError === true ? "error" : "complete",
        input: row.input,
        ...(row.result === undefined ? {} : { result: row.result }),
      })
    } else {
      flush()
      display.push({ key: row.key, kind: "row", row })
    }
  }
  flush()
  return display
}

function toolCategory(name: string, input: Record<string, unknown>): ToolCategory {
  const { renderer, labelKey } = resolveToolDisplay(name, input)
  switch (renderer) {
    case "command": return "command"
    case "search": return "search"
    case "file": return labelKey === "tool.read" ? "read" : "edit"
    case "web": return labelKey === "tool.webSearch" ? "web_search" : "web_fetch"
    case "generic": return "unknown"
  }
}

function toolPrimary(category: ToolCategory, input: Record<string, unknown>): string | undefined {
  const keys = category === "command" ? ["command"]
    : category === "web_search" ? ["query"]
      : category === "web_fetch" ? ["url"]
        : category === "search" ? ["pattern", "path"]
          : ["path"]
  const value = keys.map((key) => input[key]).find((candidate): candidate is string => typeof candidate === "string" && candidate.trim() !== "")
  if (value === undefined) return undefined
  const shown = category === "read" || category === "edit" ? value.replaceAll("\\", "/").split("/").at(-1) ?? value : value
  return shown.length <= 80 ? shown : `${shown.slice(0, 79)}…`
}
