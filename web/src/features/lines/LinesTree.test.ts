import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { createApp, defineComponent, h, type App } from "vue"
import { createPinia, setActivePinia } from "pinia"
import { i18n } from "../../i18n"
import { useChatStore } from "../../stores/chat"
import { useUiStore } from "../../stores/ui"
import LinesTree from "./LinesTree.vue"

const UIcon = defineComponent({ name: "UIcon", props: { name: String }, setup: props => () => h("i", { "data-icon": props.name }) })
const UButton = defineComponent({
  name: "UButton",
  setup: (_, { attrs, slots }) => () => h("button", { type: "button", ...attrs }, slots.default?.()),
})

const mounted: App[] = []
beforeEach(() => {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  }))
})
afterEach(() => {
  while (mounted.length > 0) mounted.pop()!.unmount()
  document.body.replaceChildren()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

function mountTree(): { root: HTMLElement; chat: ReturnType<typeof useChatStore>; ui: ReturnType<typeof useUiStore> } {
  const container = document.createElement("div")
  document.body.append(container)
  const pinia = createPinia()
  setActivePinia(pinia)
  const chat = useChatStore()
  chat.views["s-1"] = {
    sessionId: "s-1",
    status: "ready",
    defaultBranchId: "worker",
    approvals: [],
    questions: [],
    methods: [],
    branches: {},
    lines: [
      { id: "main", parentId: null, purpose: "", state: { kind: "main" }, cut: null, depth: 0 },
      { id: "worker", parentId: "main", purpose: "worker", state: { kind: "waiting" }, cut: { sequence: 0, messages: 0 }, depth: 1 },
    ],
  } as unknown as (typeof chat.views)[string]
  const app = createApp({ render: () => h(LinesTree, { sessionId: "s-1" }) })
  app.use(pinia).use(i18n).component("UIcon", UIcon).component("UButton", UButton)
  app.mount(container)
  mounted.push(app)
  return { root: container, chat, ui: useUiStore(pinia) }
}

describe("LinesTree", () => {
  it("opens the root in a tab while another line is the centre, without moving the default", () => {
    const { root, chat, ui } = mountTree()
    const openRoot = vi.spyOn(chat, "ensureLine")
    const openLine = vi.spyOn(ui, "openLine")
    const defaultBranchId = chat.views["s-1"]!.defaultBranchId

    const button = root.querySelector<HTMLButtonElement>(".lines-list > li:first-child .lines-open")
    expect(button, "the root row offers a way back to its own conversation").not.toBeNull()
    button!.click()

    expect(openRoot).toHaveBeenCalledWith("s-1", "main")
    expect(openLine).toHaveBeenCalledWith("s-1", "main")
    expect(chat.views["s-1"]!.defaultBranchId).toBe(defaultBranchId)
  })
})
