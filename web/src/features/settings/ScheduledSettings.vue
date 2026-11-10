<script setup lang="ts">
import { computed, onMounted, ref, shallowRef, watch } from "vue"
import { RouterLink, useRoute, useRouter } from "vue-router"
import { ArrowLeft, Clock3, Plus, RefreshCw } from "lucide-vue-next"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
import SelectField from "../../components/SelectField.vue"
import { useSettingsText } from "../../i18n/settings-general"
import { RpcError } from "../../rpc/client"
import { readScheduledDelete, readScheduledHistory, readScheduledJob, readScheduledJobs, readScheduledRun, readScheduledSave, type InputMode, type JobInput, type Schedule, type ScheduledJob, type ScheduledRun, type Target } from "../../rpc/scheduler"
import { useConnectionStore } from "../../stores/connection"
import { useSessionsStore } from "../../stores/sessions"
import { useToastStore } from "../../stores/toast"
import { useUiStore } from "../../stores/ui"
import { bindSettingsDirty, messageFrom, reloadSettingsOnReconnect } from "./context"
import SchedulePlan from "./SchedulePlan.vue"

const s = useSettingsText()
const route = useRoute()
const router = useRouter()
const connection = useConnectionStore()
const sessions = useSessionsStore()
const ui = useUiStore()
const toast = useToastStore()
const confirm = ref<InstanceType<typeof ConfirmDialog>>()
const jobs = ref<ScheduledJob[]>()
const runs = ref<ScheduledRun[]>()
const loading = ref(false)
const busy = ref(false)
const error = ref("")
const historyError = ref("")
const historyBusy = ref(false)
const notice = ref("")
const conflict = ref(false)
const uncertain = ref(false)
const tab = ref("overview")
const query = ref(typeof route.query.q === "string" ? route.query.q : "")
const filter = ref(typeof route.query.state === "string" ? route.query.state : "all")
const workspace = ref(typeof route.query.workspace === "string" ? route.query.workspace : "")
const id = computed(() => typeof route.params.jobId === "string" ? route.params.jobId : "")
const isNew = computed(() => route.name === "settings-scheduled-new")
const editing = computed(() => isNew.value || route.name === "settings-scheduled-edit")
const opened = computed(() => editing.value || id.value !== "")
const selected = computed(() => jobs.value?.find(job => job.id === id.value))
const supported = computed(() => connection.supports("schedule.list"))
const online = computed(() => connection.phase === "connected")
const planValid = ref(true)
const planKey = ref(0)
/** The edit form: a job's name, prompt, plan, enabled state and target (Scheduler A.3). */
type Form = { name: string; prompt: string; schedule: Schedule; enabled: boolean; kind: "launch" | "session"; cwd: string; agent_id?: string; policy?: "read-only" | "workspace-write" | "full-access"; max_run_minutes: number; session_id: string; branch_id: string; input_mode: InputMode }
const currentCwd = typeof route.query.cwd === "string" ? route.query.cwd : sessions.byId.get(ui.lastSessionId ?? "")?.cwd ?? ""
function blank(cwd: string): Form {
  return { name: "", prompt: "", schedule: { cron: "0 9 * * *", tz: Intl.DateTimeFormat().resolvedOptions().timeZone }, enabled: true, kind: "launch", cwd, agent_id: undefined, policy: undefined, max_run_minutes: 60, session_id: "", branch_id: "main", input_mode: "queue" }
}
function fromJob(job: ScheduledJob): Form {
  const form = blank("")
  form.name = job.name
  form.prompt = job.prompt
  form.schedule = { ...job.schedule }
  form.enabled = job.enabled
  applyTarget(form, job.target)
  return form
}
function applyTarget(form: Form, target: Target): void {
  form.kind = target.kind
  if (target.kind === "launch") {
    form.cwd = target.cwd
    form.agent_id = target.agent_id
    form.policy = target.policy
    form.max_run_minutes = target.max_run_minutes
  } else {
    form.session_id = target.session_id
    form.branch_id = target.branch_id
    form.input_mode = target.input_mode
  }
}
function toInput(form: Form): JobInput {
  const target: Target = form.kind === "launch"
    ? { kind: "launch", cwd: form.cwd, max_run_minutes: form.max_run_minutes,
        ...(form.agent_id ? { agent_id: form.agent_id } : {}),
        ...(form.policy ? { policy: form.policy } : {}) }
    : { kind: "session", session_id: form.session_id, branch_id: form.branch_id || "main", input_mode: form.input_mode }
  return { name: form.name, prompt: form.prompt, schedule: form.schedule, target, enabled: form.enabled }
}
const baseline = shallowRef<Form>(blank(currentCwd))
const draft = ref<Form>(structuredClone(baseline.value))
watch(() => sessions.byId.get(ui.lastSessionId ?? "")?.cwd, cwd => {
  if (isNew.value && draft.value.kind === "launch" && !draft.value.cwd && cwd) {
    baseline.value = { ...baseline.value, cwd }
    draft.value.cwd = cwd
  }
})
const dirty = computed(() => editing.value && (!planValid.value || JSON.stringify(toInput(draft.value)) !== JSON.stringify(toInput(baseline.value))))
bindSettingsDirty(dirty)
const promptBytes = computed(() => new TextEncoder().encode(draft.value.prompt).length)
const listQuery = computed(() => ({ ...(query.value ? { q: query.value } : {}), ...(filter.value !== "all" ? { state: filter.value } : {}), ...(workspace.value ? { workspace: workspace.value } : {}), ...(typeof route.query.cwd === "string" ? { cwd: route.query.cwd } : {}) }))
const listLocation = computed(() => ({ name: "settings-scheduled", query: listQuery.value }))
const workspaceOf = (target: Target): string => target.kind === "launch" ? target.cwd : ""
const workspaces = computed(() => [...new Set(jobs.value?.map(job => workspaceOf(job.target)).filter(Boolean) ?? [])].sort())
const workspaceOptions = computed(() => [...new Set([...(draft.value.kind === "launch" && draft.value.cwd ? [draft.value.cwd] : []), ...sessions.workspaces.map(workspace => workspace.cwd), ...workspaces.value])].map(cwd => ({ value: cwd, label: cwd })))
const filtered = computed(() => jobs.value?.filter(job => `${job.name} ${workspaceOf(job.target)}`.toLowerCase().includes(query.value.toLowerCase()) && (!workspace.value || workspaceOf(job.target) === workspace.value) && (filter.value === "all" || (filter.value === "invalid" ? !!job.schedule_error : filter.value === "enabled" ? job.enabled : !job.enabled))))
const targetValid = computed(() => draft.value.kind === "launch" ? draft.value.cwd.trim() !== "" : draft.value.session_id.trim() !== "")
const targetKind = computed({
  get: () => draft.value.kind,
  set: (kind: string) => {
    if (kind === draft.value.kind) return
    if (kind === "session") applyTarget(draft.value, { kind: "session", session_id: "", branch_id: "main", input_mode: "queue" })
    else applyTarget(draft.value, { kind: "launch", cwd: currentCwd, max_run_minutes: 60 })
  },
})
function permitted(method: string): boolean { return online.value && connection.supports(method) && !busy.value && !loading.value && !uncertain.value }
function writeFailed(cause: unknown): void {
  conflict.value = editing.value && cause instanceof RpcError && cause.appError === "schedule_conflict"
  uncertain.value = !(cause instanceof RpcError) || !["schedule_conflict", "schedule_invalid", "schedule_not_found", "unknown_method", "service_reloading", "service_unavailable", "invalid_params"].includes(cause.appError ?? "")
  error.value = messageFrom(cause)
}
function timestamp(value?: string): string { return value ? new Date(value).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }) : "—" }
function planLabel(plan: Schedule): string {
  if ("every_minutes" in plan) return s(`Every ${plan.every_minutes} minutes`, `每隔 ${plan.every_minutes} 分钟`)
  if ("at" in plan) return s("Once: ", "仅一次：") + timestamp(plan.at)
  const daily = /^(\d+) (\d+) \* \* \*$/.exec(plan.cron)
  return (daily ? s("Every day ", "每天 ") + `${daily[2]!.padStart(2, "0")}:${daily[1]!.padStart(2, "0")}` : `Cron ${plan.cron}`) + ` · ${plan.tz ?? s("Host local time", "主机本地时区")}`
}
function targetLabel(target: Target): string {
  if (target.kind === "session") return s(`Delivers to Session ${target.session_id}, branch ${target.branch_id} (${target.input_mode})`, `投递到会话 ${target.session_id} 的分支 ${target.branch_id}（${target.input_mode}）`)
  return s(`Launches a new Session in ${target.cwd}`, `在 ${target.cwd} 新建会话`)
}
function runState(state: string): string {
  const labels: Record<string, string> = { delivering: s("Delivering", "投递中"), delivered: s("Delivered", "已投递"), failed: s("Failed", "失败"), lost: s("Lost", "结果丢失"), missed: s("Missed", "已错过"), completed: s("Completed", "已完成"), launching: s("Launching", "已接收，启动中"), submitted: s("Submitted", "已提交"), running: s("Running", "运行中"), skipped: s("Skipped", "已跳过"), cancelled: s("Cancelled", "已取消"), timed_out: s("Timed out", "已超时") }
  return labels[state] ?? state
}
async function load(): Promise<void> {
  if (loading.value || !online.value || !supported.value) return
  if (dirty.value && !await confirm.value?.open(s("Discard the draft and load the current job?", "放弃草稿并载入当前任务？"))) return
  loading.value = true
  error.value = ""
  try {
    jobs.value = await connection.client.call("schedule.list", {}, readScheduledJobs)
    if (editing.value && selected.value) {
      baseline.value = fromJob(selected.value)
      draft.value = structuredClone(baseline.value)
      planValid.value = true
      ++planKey.value
    }
    if (!isNew.value) { conflict.value = false; uncertain.value = false }
    if (tab.value === "history") await loadHistory()
  } catch (cause) { error.value = messageFrom(cause) }
  finally { loading.value = false }
}
async function loadHistory(limit = 20): Promise<void> {
  tab.value = "history"
  if (!online.value || !connection.supports("schedule.history") || historyBusy.value) return
  historyBusy.value = true
  historyError.value = ""
  try { runs.value = await connection.client.call("schedule.history", { job_id: id.value, limit }, readScheduledHistory) }
  catch (cause) { historyError.value = messageFrom(cause) }
  finally { historyBusy.value = false }
}
async function save(): Promise<void> {
  if (!permitted("schedule.save") || !planValid.value || conflict.value || !dirty.value || !targetValid.value || (!isNew.value && !selected.value)) return
  if (promptBytes.value > 32768) { error.value = s("The prompt exceeds 32 KiB.", "指令超过 32 KiB。"); return }
  const input = toInput(draft.value)
  const prior = toInput(baseline.value)
  const patch: Record<string, unknown> = {}
  for (const key of Object.keys(input) as Array<keyof JobInput>) {
    if (!isNew.value && JSON.stringify(input[key]) === JSON.stringify(prior[key])) continue
    patch[key] = input[key]
  }
  if (!Object.keys(patch).length) return
  busy.value = true
  error.value = ""
  try {
    const result = await connection.client.call("schedule.save", { job: { ...patch, ...(isNew.value ? {} : { id: id.value }) }, ...(isNew.value ? {} : { revision: selected.value!.revision }) }, readScheduledSave)
    baseline.value = fromJob(result.job)
    draft.value = structuredClone(baseline.value)
    planValid.value = true
    toast.push({ tone: "success", title: s("Task saved", "任务已保存"), description: targetLabel(result.job.target) })
    await router.replace({ name: "settings-scheduled-detail", params: { jobId: result.job.id }, query: listQuery.value })
  } catch (cause) { writeFailed(cause) }
  finally { busy.value = false }
}
async function act(job: ScheduledJob, action: "set_enabled" | "run_now" | "delete"): Promise<void> {
  const method = `schedule.${action}`
  if (!permitted(method)) return
  busy.value = true
  const question = action === "delete" ? s(`Delete “${job.name}”? Accepted runs are not stopped.`, `删除「${job.name}」？已接收的运行不会停止。`) : s(`Run “${job.name}” now? ${targetLabel(job.target)}. This does not enable a paused task and may be skipped.`, `立即运行「${job.name}」？${targetLabel(job.target)}。不会启用暂停任务，也可能被跳过。`)
  if (action !== "set_enabled" && !await confirm.value?.open(question)) { busy.value = false; return }
  error.value = ""
  notice.value = ""
  try {
    if (action === "run_now") {
      const run = await connection.client.call(method, { job_id: job.id }, readScheduledRun)
      notice.value = `${runState(run.state)}${run.detail ? `: ${run.detail}` : ""}`
    } else if (action === "set_enabled") {
      await connection.client.call(method, { job_id: job.id, enabled: !job.enabled }, readScheduledJob)
      notice.value = s("Enablement updated. Accepted runs are unchanged.", "启用状态已更新，已接收运行不变。")
    } else {
      await connection.client.call(method, { job_id: job.id }, readScheduledDelete)
      notice.value = s("Task deleted. Accepted runs are not stopped.", "任务已删除，已接收运行不会停止。")
      if (id.value) { await router.replace(listLocation.value); return }
    }
    await load()
  } catch (cause) { writeFailed(cause) }
  finally { busy.value = false }
}
reloadSettingsOnReconnect(connection, load, dirty)
onMounted(() => { void load() })
</script>

<template>
  <article class="settings-page schedule-page">
    <ConfirmDialog ref="confirm" />
    <RouterLink v-if="opened" class="schedule-back" :to="listLocation"><ArrowLeft :size="15" aria-hidden="true" />{{ s('Scheduled tasks', '定时任务') }}</RouterLink>
    <header class="schedule-header"><div><h2 data-route-heading tabindex="-1">{{ isNew ? s('New scheduled task', '新建定时任务') : editing ? s('Edit scheduled task', '编辑定时任务') : selected?.name ?? s('Scheduled tasks', '定时任务') }}</h2><p>{{ s('Runs on the CLI host, across workspaces. Closing the browser does not stop scheduling; the host must stay running.', '任务由 CLI 主机执行，跨工作区共享。关闭浏览器不影响调度；主机需要保持运行。') }}</p></div><Clock3 :size="22" class="schedule-clock" aria-hidden="true" /></header>
    <p v-if="!supported" class="ui-banner" role="status">{{ s('Scheduled tasks are not available through this Web connection. The Scheduler may be disabled or its methods not admitted for Web access.', '当前 Web 连接无法使用定时任务：Scheduler 可能未启用，或接口尚未向 Web 放行。') }}</p>
    <template v-else>
      <p v-if="!online" class="ui-banner" role="status">{{ s('Disconnected. Shown data may be stale; drafts will not be submitted automatically.', '连接已断开，显示的数据可能已过期；草稿不会自动提交。') }}</p>
      <p v-if="notice" class="ui-banner" role="status">{{ notice }}</p>
      <div v-if="error" class="ui-banner" data-tone="danger" role="alert">{{ error }}<button class="ui-button" data-size="sm" data-variant="secondary" :disabled="loading || busy || !online" @click="load">{{ conflict ? s('Discard draft and reload', '放弃草稿并重新加载') : s('Reload data', '重新读取') }}</button></div>
      <p v-if="conflict" class="settings-help">{{ s('The job changed elsewhere. Your draft is preserved. Reload the current revision before editing again; nothing was overwritten.', '任务已在别处更新，草稿已保留；载入当前版本后再编辑，不会自动覆盖。') }}</p>
      <p v-if="uncertain" class="ui-banner" data-tone="warning" role="alert">{{ s('The outcome is unknown. Do not repeat the write. Check the task list and run history first.', '操作结果未知，请勿重复写入。先检查任务列表和运行历史。') }} <RouterLink :to="listLocation">{{ s('Check tasks', '检查任务') }}</RouterLink></p>
      <p v-if="loading" role="status">{{ s('Loading…', '加载中…') }}</p>
      <template v-if="!opened">
        <div class="settings-list-toolbar"><input v-model="query" class="ui-field" data-size="md" type="search" :aria-label="s('Search name or workspace', '搜索名称或工作区')" :placeholder="s('Search name or workspace', '搜索名称或工作区')" /><div class="settings-actions"><button class="ui-button" data-size="md" data-variant="secondary" :disabled="loading || busy || !online" @click="load"><RefreshCw :size="14" aria-hidden="true" />{{ s('Refresh', '刷新') }}</button><RouterLink v-if="connection.supports('schedule.save')" class="ui-button" data-size="md" data-variant="primary" :to="{ name: 'settings-scheduled-new', query: listQuery }"><Plus :size="14" aria-hidden="true" />{{ s('New task', '新建任务') }}</RouterLink></div></div>
        <div class="schedule-filters">
          <div class="schedule-control"><span class="settings-meta">{{ s('Status', '状态') }}</span><SelectField v-model="filter" :label="s('Status', '状态')" :options="[{ value: 'all', label: s('All statuses', '全部状态') }, { value: 'enabled', label: s('Enabled', '已启用') }, { value: 'paused', label: s('Paused', '已暂停') }, { value: 'invalid', label: s('Invalid schedule', '计划无效') }]" /></div>
          <div class="schedule-control"><span class="settings-meta">{{ s('Workspace', '工作区') }}</span><SelectField v-model="workspace" :label="s('Workspace', '工作区')" searchable :options="[{ value: '', label: s('All workspaces', '全部工作区') }, ...workspaces.map(cwd => ({ value: cwd, label: cwd }))]" /></div>
        </div>
        <section v-if="jobs" class="schedule-list">
          <div v-for="job in filtered" :key="job.id" class="schedule-job"><div class="schedule-job-heading"><RouterLink :to="{ name: 'settings-scheduled-detail', params: { jobId: job.id }, query: listQuery }">{{ job.name }}</RouterLink><span class="settings-meta">{{ job.enabled ? s('Enabled', '已启用') : s('Paused', '已暂停') }}<template v-if="job.schedule_error"> · {{ s('Invalid schedule', '计划无效') }}</template><template v-if="job.disabled_reason"> · {{ s('Disabled', '已停用') }}</template></span></div><p>{{ planLabel(job.schedule) }}</p><p class="settings-meta">{{ targetLabel(job.target) }}</p><div class="schedule-run-meta"><span>{{ s('Next run', '下次执行') }}<br>{{ job.next_window ? timestamp(job.next_window) : !job.enabled ? s('Paused', '已暂停') : s('No next run', '无下次执行') }}</span><span>{{ s('Last result', '最近结果') }}<br>{{ job.last_run ? runState(job.last_run.state) + ' · ' + timestamp(job.last_run.ended_at ?? job.last_run.created_at) : s('Not run yet', '尚未运行') }}</span></div><p v-if="job.schedule_error" class="ui-field-hint" data-tone="danger">{{ job.schedule_error }}</p><p v-if="job.disabled_reason" class="ui-field-hint">{{ s('Disabled', '已停用') }}: {{ job.disabled_reason.code }} · {{ timestamp(job.disabled_reason.at) }}</p><div class="settings-actions"><button class="ui-button" data-size="sm" data-variant="secondary" :disabled="!permitted('schedule.set_enabled')" @click="act(job, 'set_enabled')">{{ job.enabled ? s('Pause', '暂停') : s('Resume', '恢复') }}</button><button class="ui-button" data-size="sm" data-variant="ghost" :disabled="!permitted('schedule.run_now')" @click="act(job, 'run_now')">{{ s('Run now', '立即运行') }}</button></div></div>
          <p v-if="!jobs.length" class="settings-empty">{{ s('No scheduled tasks yet. Create a task to get started.', '还没有定时任务，可以新建一个。') }}</p><p v-else-if="!filtered?.length" class="settings-empty">{{ s('No matching tasks.', '没有匹配的任务。') }} <button class="ui-button" data-size="sm" data-variant="ghost" @click="query = ''; filter = 'all'; workspace = ''">{{ s('Clear filters', '清除筛选') }}</button></p>
        </section>
      </template>
      <form v-else-if="editing && (isNew || selected)" @submit.prevent="save">
        <fieldset class="schedule-form" :disabled="busy || loading || !online || !connection.supports('schedule.save') || uncertain">
          <section class="settings-card settings-form"><label><span>{{ s('Name', '名称') }}</span><input v-model="draft.name" class="ui-field" data-size="md" required /></label><label><span>{{ s('Instructions to run automatically', '未来自动执行的指令') }}</span><textarea v-model="draft.prompt" class="ui-field" data-size="md" rows="4" /><small class="settings-meta">{{ promptBytes }} / 32768 {{ s('UTF-8 bytes', 'UTF-8 字节') }}</small></label><label v-if="isNew"><input v-model="draft.enabled" type="checkbox" />{{ s('Enable after creation', '创建后启用') }}</label></section>
          <SchedulePlan :key="planKey" v-model="draft.schedule" @valid="planValid = $event" />
          <details class="settings-card" open><summary>{{ s('Where it delivers', '投递到哪里') }}</summary><div class="settings-form">
            <div class="schedule-control"><span>{{ s('Target', '目标') }}</span><SelectField v-model="targetKind" :label="s('Target', '目标')" :options="[{ value: 'launch', label: s('Launch a new Session each time', '每次新建会话') }, { value: 'session', label: s('Deliver to an existing Session branch', '投递到已有会话分支') }]" /></div>
            <template v-if="draft.kind === 'launch'">
              <div class="schedule-workspace"><span>{{ s('CLI host workspace', 'CLI 主机工作区') }}</span><SelectField v-model="draft.cwd" :label="s('CLI host workspace', 'CLI 主机工作区')" :options="workspaceOptions" searchable creatable @create="draft.cwd = $event" /><small class="settings-meta">{{ s('Current workspace is preselected. Suggestions come from known sessions and scheduled tasks; you can also enter a new host directory.', '默认使用当前工作区，候选来自已有会话和定时任务；也可输入新的主机目录。') }}</small></div>
              <div class="schedule-control"><span>{{ s('Requested permission', '声明权限') }}</span><SelectField v-model="draft.policy" :label="s('Requested permission', '声明权限')" :options="[{ value: undefined, label: s('Not declared: read-only', '未声明：只读') }, { value: 'read-only', label: s('Read only', '只读') }, { value: 'workspace-write', label: s('Workspace write', '工作区读写') }, { value: 'full-access', label: s('Full access (confirmation required)', '完全访问（需要确认）') }]" /></div>
              <p class="settings-help">{{ s('The host caps the preset when the Session launches.', '会话启动时仍受主机权限上限约束。') }}</p><label><span>{{ s('Maximum run time in minutes', '最长运行分钟数') }}</span><input v-model.number="draft.max_run_minutes" class="ui-field" data-size="md" type="number" min="1" max="4294967295" step="1" required /></label><label><span>{{ s('Agent profile ID (optional)', 'Agent profile ID（可选）') }}</span><input v-model="draft.agent_id" class="ui-field" data-size="md" /></label>
            </template>
            <template v-else>
              <label><span>{{ s('Session ID', '会话 ID') }}</span><input v-model="draft.session_id" class="ui-field" data-size="md" required /></label><label><span>{{ s('Branch', '分支') }}</span><input v-model="draft.branch_id" class="ui-field" data-size="md" placeholder="main" /></label>
              <div class="schedule-control"><span>{{ s('Input mode', '输入模式') }}</span><SelectField v-model="draft.input_mode" :label="s('Input mode', '输入模式')" :options="[{ value: 'queue', label: s('Queue until the branch is idle', '排队到分支空闲') }, { value: 'steer', label: s('Steer the running turn', '插入当前轮次') }, { value: 'append', label: s('Append at the next step', '追加到下一步') }]" /></div>
              <p class="settings-help">{{ s('The saved prompt is delivered to this branch as one input, in the chosen mode.', '保存的指令会以所选模式作为一条输入投递到该分支。') }}</p>
            </template>
          </div></details>
          <p class="settings-help">{{ s('Saving applies the schedule and the target to future runs. Accepted runs are unchanged.', '保存后，后续运行按新的计划与目标执行；已接收运行不变。') }}</p>
          <footer class="settings-actions schedule-footer"><RouterLink class="ui-button" data-size="md" data-variant="secondary" :to="listLocation">{{ s('Cancel', '取消') }}</RouterLink><button class="ui-button" data-size="md" data-variant="primary" :disabled="!dirty || !planValid || !targetValid || conflict || promptBytes > 32768 || !permitted('schedule.save')">{{ isNew ? draft.enabled ? s('Create and enable', '创建并启用') : s('Create paused task', '创建为暂停') : s('Save changes', '保存更改') }}</button></footer>
        </fieldset>
      </form>
      <template v-else-if="selected">
        <div class="settings-list-toolbar"><div class="settings-actions"><button class="ui-button" data-size="md" :data-variant="tab === 'overview' ? 'secondary' : 'ghost'" :aria-pressed="tab === 'overview'" @click="tab = 'overview'">{{ s('Overview', '概览') }}</button><button class="ui-button" data-size="md" :data-variant="tab === 'history' ? 'secondary' : 'ghost'" :aria-pressed="tab === 'history'" :disabled="!connection.supports('schedule.history')" @click="loadHistory()">{{ s('Run history', '运行历史') }}</button></div><button class="ui-button" data-size="md" data-variant="ghost" :disabled="loading || !online" @click="load">{{ s('Refresh', '刷新') }}</button></div>
        <template v-if="tab === 'overview'"><section class="settings-card"><p>{{ planLabel(selected.schedule) }}</p><p>{{ selected.enabled ? s('Enabled', '已启用') : s('Paused', '已暂停') }} · {{ s('Next run: ', '下次执行：') }}{{ timestamp(selected.next_window) }}</p><p v-if="selected.schedule_error" class="ui-field-hint" data-tone="danger">{{ selected.schedule_error }}</p><p v-if="selected.disabled_reason" class="ui-field-hint">{{ s('Disabled', '已停用') }}: {{ selected.disabled_reason.code }} · {{ timestamp(selected.disabled_reason.at) }}</p><p><code>{{ targetLabel(selected.target) }}</code></p><pre class="schedule-prompt">{{ selected.prompt }}</pre><p>{{ s('Origin', '来源') }}: {{ selected.created_by === 'user' ? s('User', '用户') : s('Tool', '工具') }}</p><details><summary>{{ s('Saved target and origin', '已保存的目标和来源') }}</summary><pre>{{ JSON.stringify({ target: selected.target, origin: selected.created_by, disabled_reason: selected.disabled_reason }, null, 2) }}</pre></details></section><div class="settings-actions"><RouterLink v-if="connection.supports('schedule.save')" class="ui-button" data-size="md" data-variant="primary" :to="{ name: 'settings-scheduled-edit', params: { jobId: id }, query: listQuery }">{{ s('Edit task', '编辑任务') }}</RouterLink><button class="ui-button" data-size="md" data-variant="secondary" :disabled="!permitted('schedule.set_enabled')" @click="act(selected, 'set_enabled')">{{ selected.enabled ? s('Pause', '暂停') : s('Resume', '恢复') }}</button><button class="ui-button" data-size="md" data-variant="secondary" :disabled="!permitted('schedule.run_now')" @click="act(selected, 'run_now')">{{ s('Run now', '立即运行') }}</button><button class="ui-button" data-size="md" data-variant="danger-ghost" :disabled="!permitted('schedule.delete')" @click="act(selected, 'delete')">{{ s('Delete task', '删除任务') }}</button></div></template>
        <section v-else class="schedule-history"><p class="settings-help">{{ s('Recent runs, newest first. Times shown in ', '最近运行，新到旧。展示时区：') }}{{ Intl.DateTimeFormat().resolvedOptions().timeZone }}</p><p v-if="historyBusy" role="status">{{ s('Loading…', '加载中…') }}</p><p v-if="historyError" class="ui-banner" data-tone="danger" role="alert">{{ historyError }}</p><div v-for="run in runs" :key="run.run_id" class="schedule-job"><div class="schedule-job-heading"><span>{{ runState(run.state) }}</span><time :datetime="run.created_at" class="settings-meta">{{ timestamp(run.created_at) }}</time></div><p v-if="run.detail">{{ run.detail }}</p><p v-if="run.ended_at" class="settings-meta">{{ s('Ended', '结束') }}: {{ timestamp(run.ended_at) }}</p><details><summary>{{ s('Run details', '运行详情') }}</summary><p class="settings-meta">{{ run.run_id }}</p><pre v-if="run.policy">{{ JSON.stringify(run.policy, null, 2) }}</pre><RouterLink v-if="run.session_id" :to="{ name: 'chat-session', params: { sessionId: run.session_id } }">{{ s('Open session', '打开会话') }}</RouterLink></details></div><p v-if="runs && !runs.length" class="settings-empty">{{ s('No runs recorded.', '没有运行记录。') }}</p><button class="ui-button" data-size="md" data-variant="secondary" :disabled="historyBusy || !online" @click="loadHistory(50)">{{ s('Read latest 50 runs', '读取最近 50 次运行') }}</button></section>
      </template>
      <p v-else-if="opened && jobs && !loading" class="ui-banner" role="status">{{ s('This task no longer exists. Return to the task list.', '任务已不存在，请返回列表。') }}</p>
    </template>
  </article>
</template>

<style scoped>
.schedule-header, .schedule-job-heading { display: flex; justify-content: space-between; align-items: start; gap: var(--space-3); }
.schedule-clock { color: var(--muted-foreground); flex: none; margin-top: 4px; }
.schedule-back { display: inline-flex; align-items: center; gap: var(--space-2); margin-bottom: var(--space-4); color: var(--muted-foreground); text-decoration: none; font-size: var(--text-md); }
.schedule-filters, .schedule-run-meta { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--space-4); }
.schedule-filters { margin-bottom: var(--space-4); }
.schedule-control { min-width: 0; display: grid; gap: var(--space-2); font-size: var(--text-md); }
.schedule-control > span:not(.settings-meta) { font-weight: var(--weight-medium); }
.schedule-list { border-top: 1px solid var(--border); }
.schedule-job { padding-block: var(--space-5); border-bottom: 1px solid var(--border); }
.schedule-job-heading a { color: var(--foreground); font-weight: var(--weight-medium); text-decoration: none; overflow-wrap: anywhere; }
.schedule-job-heading a:hover { text-decoration: underline; }
.schedule-job p { margin-block: var(--space-2); font-size: var(--text-md); overflow-wrap: anywhere; }
.schedule-run-meta { margin-block: var(--space-3); font-size: var(--text-sm); color: var(--muted-foreground); }
.schedule-form { min-width: 0; margin: 0; padding: 0; border: 0; }
.schedule-workspace { display: grid; gap: var(--space-2); font-size: var(--text-md); }
.schedule-footer { margin-top: var(--space-5); justify-content: flex-end; }
.schedule-prompt, pre { white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; }
.schedule-prompt { padding-block: var(--space-4); border-block: 1px solid var(--border); }
.schedule-history > button { margin-top: var(--space-4); }
.settings-list-toolbar > input { flex: 1 1 180px; min-width: 0; }
@media (max-width: 719px) { .schedule-filters, .schedule-run-meta { grid-template-columns: minmax(0, 1fr); } .schedule-page :is(button, .ui-button) { min-height: 44px; } }
</style>
