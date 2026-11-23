import { ref, shallowRef } from "@vue/reactivity"
import { defineStore } from "#workspace-store"
import { RpcClient, type ConnectionPhase } from "../rpc/client"
import type { HelloResult } from "../rpc/protocol"
import { useChatStore } from "./chat"
import { useSessionsStore } from "./sessions"
import { useToastStore } from "./toast"
import { useExtensionsStore } from "./extensions"
import { translate } from "#workspace-i18n"

/** The one App Server connection of this tab and its handshake state. */
export const useConnectionStore = defineStore("connection", () => {
  const phase = ref<ConnectionPhase>("connecting")
  const hello = shallowRef<HelloResult>()
  const everConnected = ref(false)

  const client = new RpcClient({
    phase(next) {
      phase.value = next
      if (next !== "connected") useExtensionsStore().clear()
    },
    async ready(result, reconnected) {
      hello.value = result
      everConnected.value = true
      void useExtensionsStore().refresh()
      if (reconnected) await useChatStore().resync()
      void useSessionsStore().refresh()
    },
    notification(notification) {
      if (notification.method === "ui.extensions.changed") { void useExtensionsStore().refresh(true); return }
      if (notification.method === "session.title") useSessionsStore().setTitle(notification.sessionId, notification.data)
      else useChatStore().receive(notification)
    },
    invalid(error) {
      console.error(error)
      useToastStore().push({ tone: "danger", title: translate("connection.protocolError"), description: error.message })
    },
  })

  /** Whether this connection may call `method` (hello's feature list). */
  function supports(method: string): boolean {
    return hello.value?.features.includes(method) ?? false
  }

  return { phase, hello, everConnected, client, supports }
})
