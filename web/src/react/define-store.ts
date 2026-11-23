import { reactive, type UnwrapNestedRefs } from "@vue/reactivity"

export function defineStore<T extends object>(_id: string, setup: () => T): () => UnwrapNestedRefs<T> {
  let instance: UnwrapNestedRefs<T> | undefined
  return () => instance ??= reactive(setup())
}
