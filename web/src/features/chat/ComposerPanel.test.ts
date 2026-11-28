import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { createApp, defineComponent, h, nextTick, type App } from "vue"
import { createPinia, setActivePinia } from "pinia"
import { i18n } from "../../i18n"
import { useConnectionStore } from "../../stores/connection"
import ComposerPanel from "./ComposerPanel.vue"

vi.mock("vue-router", () => ({
  useRoute: () => ({ params: {} }),
  useRouter: () => ({ push: vi.fn() }),
}))

vi.mock("./ComposerModelMenu.vue", async () => {
  const { defineComponent, h, ref } = await import("vue")
  return {
    default: defineComponent({
      name: "ComposerModelMenu",
      emits: ["select", "follow", "retry", "close"],
      setup(_, { emit, expose }) {
        const open = ref(false)
        expose({ open: () => { open.value = true } })
        return () => open.value
          ? h("div", { class: "stub-model-panel" }, [
              h("button", { class: "stub-model-select", onClick: () => emit("select", { providerId: "p", modelId: "m" }) }, "select"),
              h("button", { class: "stub-model-close", onClick: () => emit("close") }, "close"),
            ])
          : null
      },
    }),
  }
})

vi.mock("./ComposerPermissionMenu.vue", async () => {
  const { defineComponent, h, ref } = await import("vue")
  return {
    default: defineComponent({
      name: "ComposerPermissionMenu",
      emits: ["select", "close"],
      setup(_, { expose }) {
        const open = ref(false)
        expose({ open: () => { open.value = true } })
        return () => open.value
          ? h("div", { class: "stub-permission-panel" }, [
              h("button", { class: "stub-permission-close", onClick: () => open.value = false }, "close"),
            ])
          : null
      },
    }),
  }
})

vi.mock("./ComposerToolModeMenu.vue", async () => {
  const { defineComponent, h, ref } = await import("vue")
  return {
    default: defineComponent({
      name: "ComposerToolModeMenu",
      emits: ["select", "close"],
      setup(_, { expose }) {
        const open = ref(false)
        expose({ open: () => { open.value = true } })
        return () => open.value ? h("div", { class: "stub-tool-mode-panel" }) : null
      },
    }),
  }
})

const UIcon = defineComponent({
  name: "UIcon",
  props: { name: String },
  setup: props => () => h("i", { "data-icon": props.name }),
})
const UButton = defineComponent({
  name: "UButton",
  inheritAttrs: false,
  setup: (_, { attrs, slots }) => () => h("button", { type: "button", ...attrs }, slots.default?.()),
})
const UTooltip = defineComponent({
  name: "UTooltip",
  setup: (_, { slots }) => () => h("span", slots.default?.()),
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
  vi.stubGlobal("ResizeObserver", class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  })
})

afterEach(() => {
  while (mounted.length > 0) mounted.pop()!.unmount()
  document.body.replaceChildren()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

function mountComposer(): { root: HTMLElement; field: HTMLTextAreaElement } {
  const container = document.createElement("div")
  document.body.append(container)
  const pinia = createPinia()
  setActivePinia(pinia)
  useConnectionStore().hello = { features: ["provider.models.query"] } as never
  const app = createApp({
    render: () => h(ComposerPanel, {
      running: false,
      model: undefined,
      modelSelectable: true,
      send: async () => {},
      selectModel: async () => {},
      permission: { policy: undefined, select: async () => {} },
      toolMode: { state: undefined, available: [], select: async () => {} },
    }),
  })
  app.use(pinia).use(i18n)
  app.component("UIcon", UIcon)
  app.component("UButton", UButton)
  app.component("UTooltip", UTooltip)
  app.mount(container)
  mounted.push(app)
  const field = container.querySelector<HTMLTextAreaElement>("textarea")
  expect(field, "the composer field mounts").not.toBeNull()
  field!.focus()
  return { root: container, field: field! }
}

async function setText(field: HTMLTextAreaElement, text: string): Promise<void> {
  field.value = text
  field.dispatchEvent(new Event("input", { bubbles: true }))
  await nextTick()
}

async function press(field: HTMLTextAreaElement, key: string): Promise<void> {
  field.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }))
  await nextTick()
  await Promise.resolve()
  await nextTick()
}

function menu(root: HTMLElement): HTMLElement | null {
  return root.querySelector<HTMLElement>('[role="listbox"]')
}

function rows(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>('[role="option"]')]
}

function rowNamed(root: HTMLElement, name: string): HTMLElement | undefined {
  return rows(root).find(row => row.textContent?.includes(name))
}

describe("ComposerPanel command menu Escape", () => {
  it("clears a narrowed filter before it dismisses the menu", async () => {
    const { root, field } = mountComposer()
    await setText(field, "/mod")
    expect(menu(root), "the narrowed list is open").not.toBeNull()

    await press(field, "Escape")
    expect(field.value, "the filter is cleared and the draft kept").toBe("/")
    expect(menu(root), "the unfiltered list stays open").not.toBeNull()
    expect(rowNamed(root, "/settings"), "the parent list shows every command").toBeDefined()

    await press(field, "Escape")
    expect(menu(root), "the unfiltered root closes the menu").toBeNull()
    expect(field.value, "closing the menu keeps the draft").toBe("/")
  })

  it("returns from a panel root to the list and its selected row", async () => {
    const { root, field } = mountComposer()
    await setText(field, "/")
    await press(field, "ArrowDown")
    expect(rowNamed(root, "/permissions")?.getAttribute("aria-selected")).toBe("true")

    await press(field, "Enter")
    expect(root.querySelector(".stub-permission-panel"), "the permission panel opens").not.toBeNull()
    expect(field.value).toBe("")

    root.querySelector<HTMLButtonElement>(".stub-permission-close")!.click()
    await nextTick()
    expect(menu(root), "the command list returns from the panel root").not.toBeNull()
    expect(field.value, "the list query is restored").toBe("/")
    expect(
      rowNamed(root, "/permissions")?.getAttribute("aria-selected"),
      "the selected row is restored",
    ).toBe("true")
  })

  it("keeps a narrowed list query when its panel closes", async () => {
    const { root, field } = mountComposer()
    await setText(field, "/mod")
    await press(field, "Enter")
    expect(root.querySelector(".stub-model-panel"), "the model panel opens").not.toBeNull()

    root.querySelector<HTMLButtonElement>(".stub-model-close")!.click()
    await nextTick()
    expect(field.value, "the query that opened the panel is restored").toBe("/mod")
    expect(rowNamed(root, "/models")?.getAttribute("aria-selected")).toBe("true")
  })

  it("does not return to the list when the panel chose a value", async () => {
    const { root, field } = mountComposer()
    await setText(field, "/mod")
    await press(field, "Enter")
    root.querySelector<HTMLButtonElement>(".stub-model-select")!.click()
    await nextTick()
    root.querySelector<HTMLButtonElement>(".stub-model-close")!.click()
    await nextTick()
    expect(field.value).toBe("")
    expect(menu(root), "a chosen value closes the menu path").toBeNull()
  })
})
