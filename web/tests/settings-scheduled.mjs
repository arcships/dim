import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { readFileSync } from "node:fs"
import { registerHooks, stripTypeScriptTypes } from "node:module"
import test from "node:test"
import { compileScript, parse } from "vue/compiler-sfc"
import { createRenderer, nextTick, reactive, ref } from "vue"

globalThis.Document = class {}
globalThis.ShadowRoot = class {}
globalThis.window = { innerWidth: 1280, innerHeight: 900, addEventListener() {}, removeEventListener() {} }
const components = /\/(ScheduledSettings|SchedulePlan|SelectField|ConfirmDialog)\.vue$/
registerHooks({
  resolve(specifier, context, next) {
    if (specifier.startsWith("../../stores/")) {
      const store = specifier.split("/").at(-1)
      return { url: `data:text/javascript,export const use${store[0].toUpperCase()}${store.slice(1)}Store=()=>globalThis.scheduleStores.${store}`, shortCircuit: true }
    }
    if (specifier === "vue-i18n") return { url: "data:text/javascript,export const useI18n=()=>({t:key=>key,locale:{value:'en'}})", shortCircuit: true }
    if (specifier === "vue-router") return { url: `data:text/javascript,import{h}from'${import.meta.resolve("vue")}';export const useRoute=()=>globalThis.scheduleRoute;export const useRouter=()=>({replace:async to=>globalThis.scheduleNavigations.push(to)});export const RouterLink={props:['to'],setup:(props,{slots})=>()=>h('a',{to:props.to},slots.default?.())}`, shortCircuit: true }
    try { return next(specifier, context) } catch (error) {
      if (specifier.startsWith(".") && !/\.(ts|vue)$/.test(specifier)) return next(`${specifier}.ts`, context)
      throw error
    }
  },
  load(url, context, next) {
    if (url.endsWith(".ts")) return { format: "module", source: stripTypeScriptTypes(readFileSync(new URL(url), "utf8"), { mode: "transform" }), shortCircuit: true }
    if (!components.test(url)) return next(url, context)
    const { descriptor } = parse(readFileSync(new URL(url), "utf8"))
    return { format: "module", source: stripTypeScriptTypes(compileScript(descriptor, { id: "schedule-flow", inlineTemplate: true }).content, { mode: "transform" }), shortCircuit: true }
  },
})
const { default: ScheduledSettings } = await import("../src/features/settings/ScheduledSettings.vue")
const { RpcError } = await import("../src/rpc/client.ts")
const { settingsDirtyKey } = await import("../src/features/settings/context.ts")
const node = (type, text = "") => ({ type, text, children: [], props: {}, parent: undefined, value: "", style: {}, addEventListener() {}, focus() {}, matches() { return false }, getBoundingClientRect() { return { left: 20, top: 20, bottom: 60, width: 300, height: 120 } }, querySelector() {}, showPopover() {}, hidePopover() {}, getRootNode() { return {} }, showModal() { this.props.open = true }, close() { this.props.open = false } })
const renderer = createRenderer({
  createElement: type => node(type), createText: text => node("text", text), createComment: text => node("comment", text),
  patchProp(element, key, _, value) { element.props[key] = value; if (key === "value") { element.value = value; element._value = value } },
  setText(element, text) { element.text = text }, setElementText(element, text) { element.text = text; element.children = [] },
  parentNode: element => element.parent, nextSibling: element => element.parent?.children[element.parent.children.indexOf(element) + 1],
  insert(element, parent, anchor) { if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1); parent.children.splice(anchor ? parent.children.indexOf(anchor) : parent.children.length, 0, element); element.parent = parent },
  remove(element) { if (element.parent) element.parent.children.splice(element.parent.children.indexOf(element), 1); element.parent = undefined },
})
const all = element => [element, ...element.children.flatMap(all)]
const text = element => (element.type === "comment" ? "" : element.text) + element.children.map(text).join("")
const flush = async () => { for (let i = 0; i < 12; i++) { await Promise.resolve(); await nextTick() } }
const button = (root, label) => all(root).find(el => el.type === "button" && text(el) === label)
const field = (root, label) => all(root).find(el => el.type === "label" && text(el).startsWith(label))?.children.find(el => ["input", "textarea", "select"].includes(el.type))
const dropdown = (root, label) => all(root).find(el => el.props.role === "combobox" && el.props["aria-label"] === label)
async function select(root, label, option) {
  const trigger = dropdown(root, label)
  await trigger.props.onClick()
  await flush()
  const item = all(trigger.parent).find(el => el.props.role === "option" && text(el) === option)
  assert.ok(item, `Expected ${option} in ${label}`)
  await item.props.onClick()
  await flush()
}
async function change(element, value, event = "onChange") {
  element.props["onUpdate:modelValue"](value)
  if (element.props[event]) await element.props[event]({ target: { value } })
  await flush()
}
const submit = root => all(root).find(el => el.type === "form").props.onSubmit({ preventDefault() {} })
const policy = { preset: "read-only", allowedTools: [], disallowedTools: [] }
const job = (extra = {}) => ({ id: "job-1", name: "Audit", prompt: "Review changes", schedule: { cron: "7 */3 * * 1-5" }, target: { kind: "launch", cwd: "/old-project", max_run_minutes: 23 }, enabled: true, revision: 9, created_by: "user", next_window: null, last_run: null, ...extra })
const saved = job => ({ job })
function mount(t, route, call, supports = () => true) {
  globalThis.scheduleRoute = { name: "settings-scheduled-edit", params: { jobId: "job-1" }, query: {}, ...route }
  globalThis.scheduleNavigations = []
  globalThis.scheduleStores = {
    connection: reactive({ phase: "connected", supports, client: { call: async (method, params, read) => read(await call(method, params)) } }),
    models: reactive({ available: false, providers: [] }), ui: { lastSessionId: "current" },
    sessions: { byId: new Map([["current", { cwd: "/current-project" }]]), workspaces: [{ cwd: "/current-project" }, { cwd: "/other-project" }] },
    toast: { push() {} },
  }
  const root = node("root"), app = renderer.createApp(ScheduledSettings)
  app.provide(settingsDirtyKey, ref(false))
  app.mount(root)
  t.after(() => app.unmount())
  return root
}

test("editing changes only the requested fields and sends a changed target whole", async t => {
  const writes = []
  const stored = () => job({ target: { kind: "launch", cwd: "/old-project", agent_id: "saved-agent", max_run_minutes: 23 } })
  const root = mount(t, {}, async (method, params) => {
    if (method === "schedule.list") return { jobs: [stored()] }
    writes.push(params)
    return saved({ ...stored(), ...params.job, revision: 10 })
  })
  await flush()
  assert.equal(button(root, "Save changes").props.disabled, true)
  assert.match(text(root), /Custom Cron/)
  await change(field(root, "Name"), "Renamed", "onInput")
  await change(field(root, "Agent profile ID (optional)"), "", "onInput")
  await submit(root)
  assert.deepEqual(writes, [{ job: { id: "job-1", name: "Renamed", target: { kind: "launch", cwd: "/old-project", max_run_minutes: 23 } }, revision: 9 }])
  assert.equal(globalThis.scheduleNavigations[0].name, "settings-scheduled-detail")
})

test("new tasks use the current workspace and compile selected weekdays and minute time without owner fields", async t => {
  const writes = []
  const root = mount(t, { name: "settings-scheduled-new", params: {} }, async (method, params) => {
    if (method === "schedule.list") return { jobs: [job()] }
    writes.push(params)
    return saved(job(params.job))
  })
  await flush()
  assert.equal(text(dropdown(root, "CLI host workspace")), "/current-project")
  assert.deepEqual(all(dropdown(root, "CLI host workspace").parent).filter(el => el.props.role === "option").map(text), ["/current-project", "/other-project", "/old-project"])
  await change(field(root, "Name"), "Weekly", "onInput")
  await select(root, "CLI host workspace", "/other-project")
  await select(root, "Frequency", "Every week")
  const checkbox = all(root).find(el => el.type === "input" && el.props.type === "checkbox" && el.props.value === 2)
  await change(checkbox, [2, 6])
  await change(field(root, "Run time"), "17:43", "onInput")
  await submit(root)
  assert.equal(writes.length, 1)
  assert.deepEqual(writes[0], { job: { name: "Weekly", prompt: "", schedule: { cron: "43 17 * * 2,6", tz: Intl.DateTimeFormat().resolvedOptions().timeZone }, target: { kind: "launch", cwd: "/other-project", max_run_minutes: 60 }, enabled: true } })
})

test("one-off and interval editors submit only the selected plan with minute precision", async t => {
  for (const once of [true, false]) {
    let written
    const root = mount(t, { name: "settings-scheduled-new", params: {} }, async (method, params) => {
      if (method === "schedule.list") return { jobs: [] }
      written = params.job
      return saved(job(params.job))
    })
    await flush()
    await change(field(root, "Name"), "Plan", "onInput")
    if (once) {
      await button(root, "Once").props.onClick()
      await flush()
      await change(field(root, "Date and time"), "2026-11-05T13:27", "onInput")
    } else {
      await select(root, "Plan", "Every N minutes")
      await change(field(root, "Interval in minutes"), 73, "onInput")
    }
    await submit(root)
    assert.deepEqual(written.schedule, once ? { at: new Date(2026, 10, 5, 13, 27).toISOString() } : { every_minutes: 73 })
  }
})

test("run, pause and delete use their own management actions without authorizing saves", async t => {
  const calls = []
  let current = job()
  const root = mount(t, { name: "settings-scheduled-detail" }, async (method, params) => {
    if (method === "schedule.list") return { jobs: [current] }
    calls.push([method, params])
    if (method === "schedule.run_now") return { run_id: "accepted", job_id: current.id, state: "delivering", created_at: "2026-11-05T05:27:00Z", detail: "" }
    if (method === "schedule.set_enabled") { current = { ...current, enabled: params.enabled }; return { job: current } }
    if (method === "schedule.delete") return { job_id: current.id }
    assert.fail(`Unexpected management action ${method}`)
  })
  await flush()
  const running = button(root, "Run now").props.onClick()
  await flush()
  button(root, "confirm").props.onClick()
  await running
  await flush()
  assert.match(text(root), /Delivering/)
  await button(root, "Pause").props.onClick()
  await flush()
  const deleting = button(root, "Delete task").props.onClick()
  await flush()
  button(root, "confirm").props.onClick()
  await deleting
  assert.deepEqual(calls, [["schedule.run_now", { job_id: "job-1" }], ["schedule.set_enabled", { job_id: "job-1", enabled: false }], ["schedule.delete", { job_id: "job-1" }]])
  assert.equal(globalThis.scheduleNavigations[0].name, "settings-scheduled")
})

test("conflicts preserve the draft and a timeout cannot repeat a create", async t => {
  for (const [failure, route] of [[new RpcError(-32000, "changed", "schedule_conflict"), {}], [new Error("timed out; outcome unknown"), { name: "settings-scheduled-new", params: {} }]]) {
    let writes = 0
    const root = mount(t, route, async method => {
      if (method === "schedule.list") return { jobs: [job()] }
      writes++
      throw failure
    })
    await flush()
    await change(field(root, "Name"), "Keep this draft", "onInput")
    await submit(root)
    await submit(root)
    assert.equal(writes, 1)
    assert.equal(field(root, "Name").value, "Keep this draft")
    assert.match(text(root), failure instanceof RpcError ? /draft is preserved/ : /outcome is unknown/)
  }
})

test("missing Web admission never falls back to Settings or file reads", async t => {
  const root = mount(t, { name: "settings-scheduled", params: {} }, () => assert.fail("No method may bypass missing admission"), () => false)
  await flush()
  assert.match(text(root), /not admitted for Web access/)
  assert.equal(button(root, "New task"), undefined)
})

test("a new task can deliver into an existing Session branch", async t => {
  let written
  const root = mount(t, { name: "settings-scheduled-new", params: {} }, async (method, params) => {
    if (method === "schedule.list") return { jobs: [] }
    written = params.job
    return saved(job(params.job))
  })
  await flush()
  await change(field(root, "Name"), "Deliver", "onInput")
  await select(root, "Target", "Deliver to an existing Session branch")
  await change(field(root, "Session ID"), "session-9", "onInput")
  await submit(root)
  assert.deepEqual(written.target, { kind: "session", session_id: "session-9", branch_id: "main", input_mode: "queue" })
})

test("one-off DST gaps and repeated minutes are explicit and do not leak into repeat plans", async t => {
  if (process.env.TZ !== "America/New_York") {
    const result = spawnSync(process.execPath, ["--test", "--test-name-pattern", "one-off DST", import.meta.filename], { env: { ...process.env, TZ: "America/New_York", NODE_TEST_CONTEXT: undefined }, encoding: "utf8", timeout: 10_000 })
    assert.equal(result.status, 0, result.stdout + result.stderr)
    return
  }
  for (const next of ["Once", "Repeat", "Every N minutes", "Custom Cron"]) {
    let written
    const root = mount(t, { name: "settings-scheduled-new", params: {} }, async (method, params) => {
      if (method === "schedule.list") return { jobs: [] }
      written = params.job
      return saved(job(params.job))
    })
    await flush()
    await change(field(root, "Name"), "DST", "onInput")
    await button(root, "Once").props.onClick()
    await flush()
    await change(field(root, "Date and time"), "2027-03-14T02:30", "onInput")
    await submit(root)
    assert.equal(written, undefined)
    assert.match(text(root), /local minute does not exist/)
    await change(field(root, "Date and time"), "2026-11-01T01:30", "onInput")
    await submit(root)
    assert.equal(written, undefined)
    assert.match(text(root), /minute occurs twice/)
    await change(field(root, "Exact instant with UTC offset"), "2026-11-01T01:30:00-05:00")
    if (next === "Repeat") await button(root, next).props.onClick()
    else if (next !== "Once") await select(root, "Plan", next)
    await flush()
    if (next !== "Once") assert.equal(field(root, "Exact instant with UTC offset"), undefined)
    await submit(root)
    assert.deepEqual(written.schedule, next === "Once" ? { at: "2026-11-01T06:30:00.000Z" } : next === "Every N minutes" ? { every_minutes: 60 } : { cron: "0 9 * * *", tz: "America/New_York" })
  }
})

test("history reads render owner outcomes and session links and retain entries across read failures", async t => {
  const reads = []
  let fail = false
  const root = mount(t, { name: "settings-scheduled-detail" }, async (method, params) => {
    if (method === "schedule.list") return { jobs: [job()] }
    assert.equal(method, "schedule.history")
    reads.push(params)
    if (fail) throw new Error("History temporarily unavailable")
    return { runs: [{ run_id: "run-2", job_id: "job-1", state: "missed", created_at: "2026-11-05T05:27:00Z", ended_at: "2026-11-05T05:27:00Z", detail: "previous_delivery_unsettled" }, { run_id: "run-1", job_id: "job-1", state: "delivered", created_at: "2026-11-04T05:27:00Z", ended_at: "2026-11-04T05:28:00Z", detail: "Finished audit", session_id: "result-session", policy }] }
  })
  await flush()
  await button(root, "Run history").props.onClick()
  await flush()
  assert.deepEqual(reads, [{ job_id: "job-1", limit: 20 }])
  assert.match(text(root), /Missed.*previous_delivery_unsettled.*Delivered.*Finished audit/s)
  assert.deepEqual(all(root).find(el => el.props.to?.name === "chat-session").props.to, { name: "chat-session", params: { sessionId: "result-session" } })
  fail = true
  await button(root, "Read latest 50 runs").props.onClick()
  await flush()
  assert.match(text(root), /History temporarily unavailable/)
  assert.match(text(root), /Finished audit/)
  fail = false
  await button(root, "Read latest 50 runs").props.onClick()
  await flush()
  assert.equal(text(root).includes("History temporarily unavailable"), false)
  assert.deepEqual(reads.slice(1), [{ job_id: "job-1", limit: 50 }, { job_id: "job-1", limit: 50 }])
})
