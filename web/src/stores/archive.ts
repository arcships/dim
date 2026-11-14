import { ref } from "vue"
import { defineStore } from "pinia"

const STORAGE_KEY = "dim-web.archive.v1"

/** localStorage is outside the app: read its current value at every boundary that needs it. */
function readHidden(): string[] | undefined {
  let raw: string | null
  try {
    raw = localStorage.getItem(STORAGE_KEY)
  } catch {
    return undefined
  }
  try {
    const value: unknown = JSON.parse(raw ?? "null")
    return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : []
  } catch {
    return []
  }
}

/** The Session ids this browser hides. The daemon stores no archive state (web-workspace §4.4). */
export const useArchiveStore = defineStore("archive", () => {
  const hidden = ref<string[]>(readHidden() ?? [])

  function write(next: string[]): boolean {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(next))
      hidden.value = next
      return true
    } catch {
      return false
    }
  }

  /**
   * Applies a change to the key's current value rather than the in-memory copy.
   * Sequential changes from two open tabs are preserved; simultaneous writes
   * remain last-writer-wins because localStorage has no cross-tab compare-and-set.
   */
  function change(update: (current: string[]) => string[] | undefined): boolean {
    const current = readHidden()
    if (current === undefined) return false
    const next = update(current)
    if (next === undefined) {
      hidden.value = current
      return true
    }
    return write(next)
  }

  function has(sessionId: string): boolean {
    return hidden.value.includes(sessionId)
  }

  /** Hides the Session's row; the Session itself is untouched. */
  function archive(sessionId: string): boolean {
    return change((current) => current.includes(sessionId) ? undefined : [...current, sessionId])
  }

  /** Shows a hidden Session again. */
  function restore(sessionId: string): boolean {
    return change((current) => current.includes(sessionId) ? current.filter((id) => id !== sessionId) : undefined)
  }

  /** Drops markers the daemon does not list; no Session is deleted. */
  function drop(sessionIds: string[]): boolean {
    const stale = new Set(sessionIds)
    return change((current) => {
      const next = current.filter((id) => !stale.has(id))
      return next.length === current.length ? undefined : next
    })
  }

  if (typeof window !== "undefined") {
    window.addEventListener("storage", (event) => {
      if (event.key !== STORAGE_KEY && event.key !== null) return
      const next = readHidden()
      if (next !== undefined) hidden.value = next
    })
  }

  return { hidden, has, archive, restore, drop }
})
