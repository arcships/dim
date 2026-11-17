import { ref, watch } from "vue"
import { defineStore } from "pinia"
import type { SubmitMode } from "../rpc/protocol"

const STORAGE_KEY = "dim-web.send-mode.v1"

/** localStorage is outside the app: read it once, and only a saved `queue` changes the default. */
function readMode(): SubmitMode {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null")
    if (value === null || typeof value !== "object") return "steer"
    const stored = value as Record<string, unknown>
    return stored.sendMode === "queue" ? "queue" : "steer"
  } catch {
    return "steer"
  }
}

/**
 * How the composers send a draft typed during a run, chosen for this browser
 * (web-workspace §4, #1601). The TUI keeps the same choice in the CLI home, so
 * the two are independent, as theme and font size already are.
 */
export const useComposerStore = defineStore("composer", () => {
  const sendMode = ref<SubmitMode>(readMode())
  const error = ref("")

  watch(sendMode, (mode) => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ sendMode: mode }))
      error.value = ""
    } catch {
      error.value = "Browser storage is unavailable. The send mode will not survive a reload."
    }
  })

  /** The mode a draft typed during a run goes out in; the other one stays the alternate action. */
  function alternate(): SubmitMode {
    return sendMode.value === "steer" ? "queue" : "steer"
  }

  return { sendMode, alternate, error }
})
