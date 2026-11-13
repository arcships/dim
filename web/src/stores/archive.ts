import { ref } from "vue"
import { defineStore } from "pinia"

const STORAGE_KEY = "dim-web.archive.v1"

/** localStorage is outside the app: read it once, keeping the ids that are well-formed. */
function readHidden(): string[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null")
    return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : []
  } catch {
    return []
  }
}

/** The Session ids this browser hides. The daemon stores no archive state (web-workspace §4.4). */
export const useArchiveStore = defineStore("archive", () => {
  const hidden = ref<string[]>(readHidden())

  function write(next: string[]): boolean {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(next))
      hidden.value = next
      return true
    } catch {
      return false
    }
  }

  function has(sessionId: string): boolean {
    return hidden.value.includes(sessionId)
  }

  /** Hides the Session's row; the Session itself is untouched. */
  function archive(sessionId: string): boolean {
    return has(sessionId) || write([...hidden.value, sessionId])
  }

  /** Shows a hidden Session again. */
  function restore(sessionId: string): boolean {
    return has(sessionId) ? write(hidden.value.filter((id) => id !== sessionId)) : true
  }

  /** Drops markers the daemon does not list; no Session is deleted. */
  function drop(sessionIds: string[]): boolean {
    const stale = new Set(sessionIds)
    return write(hidden.value.filter((id) => !stale.has(id)))
  }

  return { hidden, has, archive, restore, drop }
})
