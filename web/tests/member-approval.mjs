import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick } from "vue"
import { nuxtUiTestPlugin } from "./helpers/nuxt-ui.mjs"

registerHooks({
  resolve(specifier, context, next) {
    if (context.parentURL?.endsWith("ApprovalPrompt.vue")) {
      if (specifier === "../../stores/chat") return { url: "data:text/javascript,export function useChatStore(){return globalThis.memberApprovalChat}", shortCircuit: true }
      if (specifier === "vue-i18n") return { url: "data:text/javascript,export function useI18n(){return {t:(key)=>key}}", shortCircuit: true }
    }
    try { return next(specifier, context) } catch (error) {
      if ((specifier.startsWith("./") || specifier.startsWith("../")) && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!url.endsWith("ApprovalPrompt.vue")) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    const script = compileScript(descriptor, { id: "member-approval-test", inlineTemplate: true })
    return { format: "module", source: stripTypeScriptTypes(script.content, { mode: "transform" }), shortCircuit: true }
  },
})

const { default: ApprovalPrompt } = await import("../src/features/chat/ApprovalPrompt.vue")
const { RpcError } = await import("../src/rpc/client.ts")
const node = (type, text = "") => ({ type, text, children: [], props: {}, parent: undefined })
const renderer = createRenderer({
  createElement: node, createText: (text) => node("text", text), createComment: (text) => node("comment", text),
  patchProp: (element, key, _, value) => { element.props[key] = value },
  setText: (element, text) => { element.text = text },
  setElementText: (element, text) => { element.text = text; element.children = [] },
  parentNode: (element) => element.parent,
  nextSibling: (element) => element.parent?.children[element.parent.children.indexOf(element) + 1],
  insert(element, parent, anchor) {
    if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1)
    const index = anchor ? parent.children.indexOf(anchor) : parent.children.length
    parent.children.splice(index, 0, element); element.parent = parent
  },
  remove(element) {
    if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1)
    element.parent = undefined
  },
})
const text = (element) => (element.type === "comment" ? "" : element.text) + element.children.map(text).join("")
const all = (element) => [element, ...element.children.flatMap(all)]
const button = (root, label) => all(root).find((element) => element.type === "button" && text(element) === label)
const flush = async () => { for (let index = 0; index < 8; index++) { await Promise.resolve(); await nextTick() } }
const original = { state: "pending", approvalId: "route", descriptor: {
  delegationId: "delegation", member: { agentId: "reviewer", sessionId: "member", branchId: "worker" }, requestId: "original",
  call: { name: "exec", input: { command: "cargo test original member action" } }, reason: "Member action",
} }
function mount(memberApproval) {
  const answers = []
  globalThis.memberApprovalChat = { memberApproval, respond: async (session, request, decision) => { answers.push({ session, request: request.requestId, decision }) } }
  const root = node("root")
  const app = renderer.createApp(ApprovalPrompt, { sessionId: "leader", keyboard: false, request: {
    branchId: "review", requestId: "outer", reason: "Leader reason", call: { name: "member_approval_allow", input: { approvalId: "route" } },
  } })
  app.use(nuxtUiTestPlugin)
  app.mount(root)
  return { root, app, answers }
}

test("member loading keeps Allow disabled and rejection available", async () => {
  let finish
  const { root, app, answers } = mount(() => new Promise((resolve) => { finish = resolve }))
  await flush()
  assert.match(text(root), /Loading member action/)
  assert.equal(button(root, "prompt.allowOnce").props.disabled, true)
  await button(root, "prompt.allowOnce").props.onClick()
  assert.deepEqual(answers, [])
  await button(root, "prompt.reject").props.onClick()
  assert.deepEqual(answers, [{ session: "leader", request: "outer", decision: "deny" }])
  app.unmount(); finish(original); await flush()
  assert.equal(answers.length, 1)
})

test("a retried member review displays the original action and rechecks before allowing", async () => {
  let reads = 0
  const { root, app, answers } = mount(async () => {
    reads++
    if (reads === 1) throw new Error("service reloading")
    return reads === 2 ? original : { state: "ended" }
  })
  await flush()
  assert.equal(button(root, "prompt.allowOnce").props.disabled, true)
  await button(root, "Retry").props.onClick(); await flush()
  assert.match(text(root), /Leader branch: review/)
  assert.match(text(root), /cargo test original member action/)
  assert.equal(button(root, "prompt.allowOnce").props.disabled, false)
  await button(root, "prompt.allowOnce").props.onClick(); await flush()
  assert.equal(reads, 3)
  assert.deepEqual(answers, [{ session: "leader", request: "outer", decision: "deny" }])
  app.unmount()
})

test("an absent member-detail method offers rejection without a futile retry", async () => {
  const { root, app, answers } = mount(async () => { throw new RpcError(-32601, "Unknown method", undefined) })
  await flush()
  assert.equal(button(root, "Retry"), undefined)
  assert.equal(button(root, "prompt.allowOnce").props.disabled, true)
  await button(root, "prompt.reject").props.onClick()
  assert.deepEqual(answers, [{ session: "leader", request: "outer", decision: "deny" }])
  app.unmount()
})
