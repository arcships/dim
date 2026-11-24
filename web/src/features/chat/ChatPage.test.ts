import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { createApp, defineComponent, h, nextTick, type App } from "vue"
import { createMemoryHistory, createRouter } from "vue-router"
import { createPinia, setActivePinia } from "pinia"
import { i18n } from "../../i18n"
import { useChatStore } from "../../stores/chat"
import ChatPage from "./ChatPage.vue"

const mocks = vi.hoisted(() => {
  const branchIds: string[] = []
  const plain = (name: string) => async () => {
    const { defineComponent, h } = await import("vue")
    return { default: defineComponent({ name, setup: () => () => h("div") }) }
  }
  return { branchIds, plain }
})

vi.mock("../../chat-ui/timeline", async () => {
  const { defineComponent, h } = await import("vue")
  return {
    WebChatTimeline: defineComponent({
      name: "WebChatTimeline",
      props: { branchId: { type: String, required: true } },
      setup(props) {
        mocks.branchIds.push(props.branchId!)
        return () => h("div", { "data-timeline-branch": props.branchId })
      },
    }),
  }
})
vi.mock("./ComposerPanel.vue", mocks.plain("ComposerPanel"))
vi.mock("./ApprovalPrompt.vue", mocks.plain("ApprovalPrompt"))
vi.mock("./QueuedInputs.vue", mocks.plain("QueuedInputs"))
vi.mock("./QuestionPrompt.vue", mocks.plain("QuestionPrompt"))
vi.mock("../extensions/ExtensionMenu.vue", mocks.plain("ExtensionMenu"))
vi.mock("../../components/BrandMark.vue", mocks.plain("BrandMark"))
vi.mock("../../components/ConfirmDialog.vue", mocks.plain("ConfirmDialog"))

const mounted: App[] = []

function simple(name: string) {
  return defineComponent({ name, setup: () => () => h("div") })
}

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
  mocks.branchIds.length = 0
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

async function mountPage(defaultBranchId: string, status: "ready" | "failed" = "ready", error = ""): Promise<HTMLElement> {
  const container = document.createElement("div")
  document.body.append(container)
  const pinia = createPinia()
  setActivePinia(pinia)
  const chat = useChatStore()
  vi.spyOn(chat, "open").mockImplementation(() => {})
  chat.views["s-1"] = {
    sessionId: "s-1",
    defaultBranchId,
    status,
    error,
    rows: [],
    running: false,
    stopping: false,
    retry: undefined,
    sessionRunning: false,
    approvals: [],
    questions: [],
    methods: [],
    permission: undefined,
    permissionVersion: undefined,
    goal: undefined,
    queued: [],
    model: undefined,
    lines: [],
    branches: {},
    lastTurnEnd: undefined,
    earlier: false,
    loadingEarlier: false,
    historyError: "",
  }
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/chat/new", name: "chat-new", component: { render: () => null } },
      { path: "/chat/:sessionId", name: "chat-session", component: { render: () => null } },
    ],
  })
  await router.push("/chat/s-1")
  const app = createApp({ render: () => h(ChatPage, { sessionId: "s-1" }) })
  app.use(pinia).use(i18n).use(router)
  app.component("UButton", defineComponent({
    name: "UButton",
    setup: (_, { attrs, slots }) => () => h("button", attrs, slots.default?.()),
  }))
  app.component("UBadge", simple("UBadge"))
  app.component("UCollapsible", defineComponent({
    name: "UCollapsible",
    setup: (_, { slots }) => () => h("div", slots.default?.()),
  }))
  app.mount(container)
  mounted.push(app)
  await nextTick()
  return container
}

describe("ChatPage", () => {
  it("tells the timeline which line the centre is", async () => {
    await mountPage("worker")
    expect(mocks.branchIds).toContain("worker")
    expect(mocks.branchIds).not.toContain("main")
  })

  it("retries the accepted line's reads from the failed page", async () => {
    const container = await mountPage("worker", "failed", "the line could not be read")
    const chat = useChatStore()
    const retry = vi.spyOn(chat, "retryCentre").mockImplementation(() => {})
    expect(container.textContent).toContain("the line could not be read")
    const button = container.querySelector("button")
    expect(button?.textContent?.trim()).toBe("Retry")
    button!.click()
    await nextTick()
    expect(retry).toHaveBeenCalledWith("s-1")
  })
})
