import { shallowReactive, watch } from "@vue/reactivity"
import { useMemo, useSyncExternalStore } from "react"
import type { RecalledImage } from "../features/chat/recall"
import type { FileMention } from "../mentions"

type Draft = {
  draft: string
  files: File[]
  pastes: string[]
  restored: RecalledImage[]
  mentions: FileMention[]
  literalSlash: boolean
  busy: boolean
  failure: string
  uploadProgress?: number
}

export const emptyDraft: Draft = { draft: "", files: [], pastes: [], restored: [], mentions: [], literalSlash: false, busy: false, failure: "" }
const drafts = shallowReactive(new Map<string, Draft>())

export function useComposerDraft(sessionId: string, branchId: string) {
  const store = useMemo(() => {
    const key = JSON.stringify([sessionId, branchId])
    const read = () => drafts.get(key) ?? emptyDraft
    return {
      read,
      subscribe: (notify: () => void) => watch(read, notify),
      write(change: Partial<Draft>) {
        const next = { ...read(), ...change }
        if (next.busy || next.draft || next.files.length || next.pastes.length || next.restored.length) drafts.set(key, next)
        else drafts.delete(key)
      },
    }
  }, [sessionId, branchId])
  return [useSyncExternalStore(store.subscribe, store.read), store] as const
}
