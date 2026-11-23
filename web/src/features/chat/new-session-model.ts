import { computed, reactive, ref } from "@vue/reactivity"
import type { ModelId } from "../../rpc/protocol"

/** How a new Session's model stands right now. */
export type ModelResolution =
  | { state: "idle" }
  | { state: "loading" }
  | { state: "resolved"; model: ModelId }
  | { state: "failed"; message: string }
  | { state: "none" }

/** What the model menu shows of that resolution, and the two actions it offers. */
export type ModelMenuPreview = {
  state: ModelResolution["state"]
  following: boolean
  /** The model following defaults resolves to, named by the menu's own lookup. */
  resolved?: ModelId
  message?: string
  retry: () => void
  follow: () => void
}

/**
 * The new-Session composer's model: the model creation would adopt in the chosen
 * directory, and the model this page chooses for one Session instead. Following
 * defaults is the absence of a choice, so a settled preview never replaces one,
 * and a reply for a directory that is no longer shown is discarded.
 */
export function useNewSessionModel(resolve: (cwd: string) => Promise<ModelId | undefined>) {
  const resolution = ref<ModelResolution>({ state: "idle" })
  const override = ref<ModelId>()
  let generation = 0
  let directory = ""

  const model = computed(() => override.value ?? (resolution.value.state === "resolved" ? resolution.value.model : undefined))
  const following = computed(() => override.value === undefined)

  async function refresh(cwd: string): Promise<void> {
    const request = ++generation
    directory = cwd
    if (cwd === "") {
      resolution.value = { state: "idle" }
      return
    }
    resolution.value = { state: "loading" }
    try {
      const resolved = await resolve(cwd)
      if (request !== generation) return
      resolution.value = resolved === undefined ? { state: "none" } : { state: "resolved", model: resolved }
    } catch (cause) {
      if (request !== generation) return
      resolution.value = { state: "failed", message: cause instanceof Error ? cause.message : String(cause) }
    }
  }

  function choose(model: ModelId): void {
    override.value = model
  }

  function follow(): void {
    override.value = undefined
  }

  const menu = computed<ModelMenuPreview>(() => ({
    state: resolution.value.state,
    following: following.value,
    ...(resolution.value.state === "resolved" ? { resolved: resolution.value.model } : {}),
    ...(resolution.value.state === "failed" ? { message: resolution.value.message } : {}),
    retry: () => void refresh(directory),
    follow,
  }))

  return reactive({ resolution, override, model, following, menu, refresh, choose, follow })
}
