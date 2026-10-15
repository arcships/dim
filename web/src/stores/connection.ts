import { ref, shallowRef } from "vue"
import { defineStore } from "pinia"
import { RpcClient, type ConnectionPhase } from "../rpc/client"
import type { HelloResult } from "../rpc/protocol"
import { useChatStore } from "./chat"
import { useSessionsStore } from "./sessions"
import { useToastStore } from "./toast"
import { translate } from "../i18n"

/** The one App Server connection of this tab and its handshake state. */
export const useConnectionStore = defineStore("connection", () => {
  const phase = ref<ConnectionPhase>("connecting")
  const hello = shallowRef<HelloResult>()
  const everConnected = ref(false)

  const client = new RpcClient({
    phase(next) {
      phase.value = next
    },
    ready(result, reconnected) {
      hello.value = result
      everConnected.value = true
      void useSessionsStore().refresh()
      if (reconnected) useChatStore().resync()
    },
    notification(notification) {
      useChatStore().receive(notification)
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
