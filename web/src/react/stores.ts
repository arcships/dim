import { watch } from "@vue/reactivity"
import { useMemo, useSyncExternalStore } from "react"
import { useAppearanceStore } from "../stores/appearance"
import { useChatStore } from "../stores/chat"
import { useConnectionStore } from "../stores/connection"
import { useSessionsStore } from "../stores/sessions"

export const appearance = useAppearanceStore()
export const connection = useConnectionStore()
export const sessions = useSessionsStore()
export const chat = useChatStore()

export function useStoreValue<T>(read: () => T): T {
  const store = useMemo(() => {
    let snapshot = read()
    return {
      read: () => snapshot,
      subscribe: (listener: () => void) => watch(read, (next) => {
        snapshot = next
        listener()
      }, { immediate: true, scheduler: queueMicrotask }),
    }
  }, [read])
  return useSyncExternalStore(store.subscribe, store.read)
}
