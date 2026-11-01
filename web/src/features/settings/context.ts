import { inject, onBeforeUnmount, watch, type InjectionKey, type Ref } from "vue"
import type { ConnectionPhase } from "../../rpc/client"

export const settingsDirtyKey: InjectionKey<Ref<boolean>> = Symbol("settings-dirty")

export function bindSettingsDirty(dirty: Readonly<Ref<boolean>>): void {
  const target = inject(settingsDirtyKey)
  if (!target) return
  watch(dirty, (value) => { target.value = value }, { immediate: true, flush: "sync" })
  onBeforeUnmount(() => { target.value = false })
}

export function reloadSettingsOnReconnect(connection: { phase: ConnectionPhase }, load: () => Promise<void>, dirty?: Readonly<Ref<boolean>>): void {
  watch(() => connection.phase, (phase, before) => {
    if (phase === "connected" && before !== "connected" && !dirty?.value) void load()
  })
}

export function messageFrom(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
