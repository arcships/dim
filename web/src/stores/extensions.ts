import { ref, shallowRef } from "@vue/reactivity"
import { defineStore } from "#workspace-store"
import type { ExtensionDirectory } from "../../../npm/ui"
import { readExtensions } from "../rpc/read"
import { useConnectionStore } from "./connection"

export const useExtensionsStore = defineStore("extensions", () => {
  const directory = shallowRef<ExtensionDirectory>()
  const error = ref("")
  let request = 0

  function clear(): void {
    request++
    directory.value = undefined
    error.value = ""
  }
  async function refresh(discovered = false): Promise<void> {
    const connection = useConnectionStore()
    if (connection.phase !== "connected") return
    if (!discovered && !connection.supports("ui.extensions.list")) { clear(); return }
    const current = ++request
    try {
      const next = await connection.client.call("ui.extensions.list", {}, readExtensions)
      if (current !== request) return
      directory.value = next
      error.value = ""
    } catch (reason) {
      if (current !== request) return
      directory.value = undefined
      error.value = reason instanceof Error ? reason.message : String(reason)
    }
  }
  return { directory, error, clear, refresh }
})
