import type { TranslationKey } from "../../i18n"

/**
 * Which input panel a tool call shows (in an approval prompt or an expanded
 * timeline row) and the action label it reads. The first match wins.
 */
export type ToolRenderer = "command" | "search" | "file" | "web" | "generic"

const NAME_LABELS: Record<string, TranslationKey> = {
  read: "tool.read",
  read_file: "tool.read",
  write: "tool.write",
  write_file: "tool.write",
  edit: "tool.edit",
  edit_file: "tool.edit",
  apply_patch: "tool.edit",
  grep: "tool.grep",
  rg: "tool.grep",
  glob: "tool.glob",
}

function normalize(toolName: string): string {
  const raw = toolName.trim().toLowerCase()
  return raw.split(/(?:__|[./\s:-])+/).filter(Boolean).at(-1) ?? raw
}

export function resolveToolDisplay(toolName: string, input: Record<string, unknown>): { renderer: ToolRenderer; labelKey: TranslationKey } {
  const raw = toolName.trim().toLowerCase()
  if (raw.startsWith("mcp__")) return { renderer: "generic", labelKey: "prompt.useTool" }
  const name = normalize(toolName)
  if (["exec", "bash", "shell", "run_command", "exec_command"].includes(name)) {
    return { renderer: "command", labelKey: input.runInBackground === true ? "tool.runBackground" : "tool.run" }
  }
  if (["grep", "rg", "glob", "find", "ls", "list_files"].includes(name)) {
    const pattern = typeof input.pattern === "string" ? input.pattern : undefined
    return { renderer: "search", labelKey: name === "glob" && (!pattern || pattern === "*") ? "tool.listFiles" : NAME_LABELS[name] ?? "tool.grep" }
  }
  if (["read", "read_file", "write", "write_file", "edit", "edit_file", "apply_patch"].includes(name)) {
    return { renderer: "file", labelKey: NAME_LABELS[name] ?? "tool.edit" }
  }
  if (raw.includes("web_search") || name === "websearch") return { renderer: "web", labelKey: "tool.webSearch" }
  if (raw.includes("web_fetch") || name === "webfetch") return { renderer: "web", labelKey: "tool.webFetch" }
  return { renderer: "generic", labelKey: "prompt.useTool" }
}

/** Display lines of an edit: the tool's unified diff when it reported one, else the replacements it carried, in the order they applied. */
export function diffLines(input: Record<string, unknown>, diff: string | undefined): Array<{ sign: "-" | "+" | " "; text: string }> {
  if (diff !== undefined) {
    return diff.split("\n").filter((line) => !line.startsWith("---") && !line.startsWith("+++") && !line.startsWith("@@")).map((line) => {
      const sign = line.startsWith("+") ? "+" : line.startsWith("-") ? "-" : " "
      return { sign, text: sign === " " ? line.replace(/^ /, "") : line.slice(1) }
    })
  }
  const replacements: Array<[unknown, unknown]> = Array.isArray(input.edits)
    ? input.edits.map((item) => {
      const edit = typeof item === "object" && item !== null ? item as Record<string, unknown> : {}
      return [edit.oldText, edit.newText]
    })
    : [[input.oldText, input.newText]]
  return replacements.flatMap(([oldText, newText]) => {
    if (typeof oldText !== "string" || typeof newText !== "string") return []
    return [
      ...oldText.split("\n").map((text) => ({ sign: "-" as const, text })),
      ...newText.split("\n").map((text) => ({ sign: "+" as const, text })),
    ]
  })
}
