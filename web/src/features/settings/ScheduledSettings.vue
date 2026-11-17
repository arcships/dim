<script setup lang="ts">
import { computed, onMounted, ref, shallowRef, watch } from "vue"
import { useRoute, useRouter } from "vue-router"
import ConfirmDialog from "../../components/ConfirmDialog.vue"
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
const ALL_WORKSPACES = "__all__"
const DEFAULT_POLICY = "__default__"
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
const workspaceFilter = computed({
  get: () => workspace.value || ALL_WORKSPACES,
  set: (value: string) => { workspace.value = value === ALL_WORKSPACES ? "" : value },
})
const statusFilterItems = computed(() => [
  { value: "all", label: s("All statuses", "全部状态") },
  { value: "enabled", label: s("Enabled", "已启用") },
  { value: "paused", label: s("Paused", "已暂停") },
  { value: "invalid", label: s("Invalid schedule", "计划无效") },
])
const workspaceFilterItems = computed(() => [
  { value: ALL_WORKSPACES, label: s("All workspaces", "全部工作区") },
  ...workspaces.value.map(cwd => ({ value: cwd, label: cwd })),
])
const workspaceOptions = computed(() => [...new Set([...(draft.value.kind === "launch" && draft.value.cwd ? [draft.value.cwd] : []), ...sessions.workspaces.map(workspace => workspace.cwd), ...workspaces.value])].map(cwd => ({ value: cwd, label: cwd })))
const targetItems = computed(() => [
  { value: "launch", label: s("Launch a new Session each time", "每次新建会话") },
  { value: "session", label: s("Deliver to an existing Session branch", "投递到已有会话分支") },
])
const policyChoice = computed({
  get: () => draft.value.policy ?? DEFAULT_POLICY,
  set: (value: string) => { draft.value.policy = value === DEFAULT_POLICY ? undefined : value as Form["policy"] },
})
const policyItems = computed(() => [
  { value: DEFAULT_POLICY, label: s("Not declared: read-only", "未声明：只读") },
  { value: "read-only", label: s("Read only", "只读") },
  { value: "workspace-write", label: s("Workspace write", "工作区读写") },
  { value: "full-access", label: s("Full access (confirmation required)", "完全访问（需要确认）") },
])
const inputModeItems = computed(() => [
  { value: "queue", label: s("Queue until the branch is idle", "排队到分支空闲") },
  { value: "steer", label: s("Steer the running turn", "插入当前轮次") },
  { value: "append", label: s("Append at the next step", "追加到下一步") },
])
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
function createWorkspace(value: unknown): void {
  if (typeof value === "string") draft.value.cwd = value
  else if (value && typeof value === "object" && "value" in value && typeof value.value === "string") draft.value.cwd = value.value
}
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
    <UButton v-if="opened" class="schedule-back" color="neutral" variant="link" icon="i-lucide-arrow-left" :to="listLocation">{{ s('Scheduled tasks', '定时任务') }}</UButton>
    <header class="schedule-header">
      <div><h2 data-route-heading tabindex="-1">{{ isNew ? s('New scheduled task', '新建定时任务') : editing ? s('Edit scheduled task', '编辑定时任务') : selected?.name ?? s('Scheduled tasks', '定时任务') }}</h2><p>{{ s('Runs on the CLI host, across workspaces. Closing the browser does not stop scheduling; the host must stay running.', '任务由 CLI 主机执行，跨工作区共享。关闭浏览器不影响调度；主机需要保持运行。') }}</p></div>
      <UIcon name="i-lucide-clock-3" class="schedule-clock size-5" aria-hidden="true" />
    </header>
    <UAlert v-if="!supported" class="settings-banner" color="neutral" variant="soft" role="status" :description="s('Scheduled tasks are not available through this Web connection. The Scheduler may be disabled or its methods not admitted for Web access.', '当前 Web 连接无法使用定时任务：Scheduler 可能未启用，或接口尚未向 Web 放行。')" />
    <template v-else>
      <UAlert v-if="!online" class="settings-banner" color="warning" variant="soft" role="status" :description="s('Disconnected. Shown data may be stale; drafts will not be submitted automatically.', '连接已断开，显示的数据可能已过期；草稿不会自动提交。')" />
      <UAlert v-if="notice" class="settings-banner" color="info" variant="soft" role="status" :description="notice" />
      <UAlert v-if="error" class="settings-banner" color="error" variant="soft" role="alert" :description="error">
        <template #actions><UButton size="sm" color="neutral" variant="soft" :disabled="loading || busy || !online" @click="load">{{ conflict ? s('Discard draft and reload', '放弃草稿并重新加载') : s('Reload data', '重新读取') }}</UButton></template>
      </UAlert>
      <p v-if="conflict" class="settings-help">{{ s('The job changed elsewhere. Your draft is preserved. Reload the current revision before editing again; nothing was overwritten.', '任务已在别处更新，草稿已保留；载入当前版本后再编辑，不会自动覆盖。') }}</p>
      <UAlert v-if="uncertain" class="settings-banner" color="warning" variant="soft" role="alert" :description="s('The outcome is unknown. Do not repeat the write. Check the task list and run history first.', '操作结果未知，请勿重复写入。先检查任务列表和运行历史。')">
        <template #actions><UButton size="sm" variant="link" :to="listLocation">{{ s('Check tasks', '检查任务') }}</UButton></template>
      </UAlert>
      <p v-if="loading" role="status">{{ s('Loading…', '加载中…') }}</p>
      <template v-if="!opened">
        <div class="settings-list-toolbar">
          <UInput v-model="query" type="search" :aria-label="s('Search name or workspace', '搜索名称或工作区')" :placeholder="s('Search name or workspace', '搜索名称或工作区')" />
          <div class="settings-actions">
            <UButton color="neutral" variant="soft" icon="i-lucide-refresh-cw" :disabled="loading || busy || !online" @click="load">{{ s('Refresh', '刷新') }}</UButton>
            <UButton v-if="connection.supports('schedule.save')" color="primary" icon="i-lucide-plus" :to="{ name: 'settings-scheduled-new', query: listQuery }">{{ s('New task', '新建任务') }}</UButton>
          </div>
        </div>
        <div class="schedule-filters">
          <UFormField :label="s('Status', '状态')">
            <USelectMenu v-model="filter" value-key="value" :search-input="false" :aria-label="s('Status', '状态')" :items="statusFilterItems" />
          </UFormField>
          <UFormField :label="s('Workspace', '工作区')">
            <USelectMenu v-model="workspaceFilter" value-key="value" :aria-label="s('Workspace', '工作区')" :items="workspaceFilterItems" />
          </UFormField>
        </div>
        <section v-if="jobs" class="schedule-list">
          <div v-for="job in filtered" :key="job.id" class="schedule-job">
            <div class="schedule-job-heading">
              <UButton variant="link" class="p-0" :to="{ name: 'settings-scheduled-detail', params: { jobId: job.id }, query: listQuery }">{{ job.name }}</UButton>
              <span class="settings-meta">{{ job.enabled ? s('Enabled', '已启用') : s('Paused', '已暂停') }}<template v-if="job.schedule_error"> · {{ s('Invalid schedule', '计划无效') }}</template><template v-if="job.disabled_reason"> · {{ s('Disabled', '已停用') }}</template></span>
            </div>
            <p>{{ planLabel(job.schedule) }}</p>
            <p class="settings-meta">{{ targetLabel(job.target) }}</p>
            <div class="schedule-run-meta">
              <span>{{ s('Next run', '下次执行') }}<br>{{ job.next_window ? timestamp(job.next_window) : !job.enabled ? s('Paused', '已暂停') : s('No next run', '无下次执行') }}</span>
              <span>{{ s('Last result', '最近结果') }}<br>{{ job.last_run ? runState(job.last_run.state) + ' · ' + timestamp(job.last_run.ended_at ?? job.last_run.created_at) : s('Not run yet', '尚未运行') }}</span>
            </div>
            <p v-if="job.schedule_error" class="schedule-error">{{ job.schedule_error }}</p>
            <p v-if="job.disabled_reason" class="settings-meta">{{ s('Disabled', '已停用') }}: {{ job.disabled_reason.code }} · {{ timestamp(job.disabled_reason.at) }}</p>
            <div class="settings-actions">
              <UButton size="sm" color="neutral" variant="soft" :disabled="!permitted('schedule.set_enabled')" @click="act(job, 'set_enabled')">{{ job.enabled ? s('Pause', '暂停') : s('Resume', '恢复') }}</UButton>
              <UButton size="sm" color="neutral" variant="ghost" :disabled="!permitted('schedule.run_now')" @click="act(job, 'run_now')">{{ s('Run now', '立即运行') }}</UButton>
            </div>
          </div>
          <p v-if="!jobs.length" class="settings-empty">{{ s('No scheduled tasks yet. Create a task to get started.', '还没有定时任务，可以新建一个。') }}</p>
          <p v-else-if="!filtered?.length" class="settings-empty">{{ s('No matching tasks.', '没有匹配的任务。') }} <UButton size="sm" color="neutral" variant="ghost" @click="query = ''; filter = 'all'; workspace = ''">{{ s('Clear filters', '清除筛选') }}</UButton></p>
        </section>
      </template>
      <form v-else-if="editing && (isNew || selected)" @submit.prevent="save">
        <fieldset class="schedule-form" :disabled="busy || loading || !online || !connection.supports('schedule.save') || uncertain">
          <section class="settings-card settings-form">
            <UFormField :label="s('Name', '名称')"><UInput v-model="draft.name" required /></UFormField>
            <UFormField :label="s('Instructions to run automatically', '未来自动执行的指令')" :description="`${promptBytes} / 32768 ${s('UTF-8 bytes', 'UTF-8 字节')}`">
              <UTextarea v-model="draft.prompt" :rows="4" />
            </UFormField>
            <UCheckbox v-if="isNew" v-model="draft.enabled" :label="s('Enable after creation', '创建后启用')" />
          </section>
          <SchedulePlan :key="planKey" v-model="draft.schedule" @valid="planValid = $event" />
          <UCollapsible default-open class="settings-card">
            <UButton size="sm" color="neutral" variant="ghost" trailing-icon="i-lucide-chevron-down" :label="s('Where it delivers', '投递到哪里')" />
            <template #content>
              <div class="settings-form schedule-deliver">
                <UFormField :label="s('Target', '目标')">
                  <USelectMenu v-model="targetKind" value-key="value" :search-input="false" :aria-label="s('Target', '目标')" :items="targetItems" />
                </UFormField>
                <template v-if="draft.kind === 'launch'">
                  <UFormField :label="s('CLI host workspace', 'CLI 主机工作区')" :description="s('Current workspace is preselected. Suggestions come from known sessions and scheduled tasks; you can also enter a new host directory.', '默认使用当前工作区，候选来自已有会话和定时任务；也可输入新的主机目录。')">
                    <USelectMenu v-model="draft.cwd" value-key="value" :create-item="{ when: 'always', position: 'bottom' }" :aria-label="s('CLI host workspace', 'CLI 主机工作区')" :items="workspaceOptions" @create="createWorkspace" />
                  </UFormField>
                  <UFormField :label="s('Requested permission', '声明权限')" :help="s('The host caps the preset when the Session launches.', '会话启动时仍受主机权限上限约束。')">
                    <USelectMenu v-model="policyChoice" value-key="value" :search-input="false" :aria-label="s('Requested permission', '声明权限')" :items="policyItems" />
                  </UFormField>
                  <UFormField :label="s('Maximum run time in minutes', '最长运行分钟数')">
                    <UInput v-model.number="draft.max_run_minutes" type="number" min="1" max="4294967295" step="1" required />
                  </UFormField>
                  <UFormField :label="s('Agent profile ID (optional)', 'Agent profile ID（可选）')">
                    <UInput v-model="draft.agent_id" />
                  </UFormField>
                </template>
                <template v-else>
                  <UFormField :label="s('Session ID', '会话 ID')"><UInput v-model="draft.session_id" required /></UFormField>
                  <UFormField :label="s('Branch', '分支')"><UInput v-model="draft.branch_id" placeholder="main" /></UFormField>
                  <UFormField :label="s('Input mode', '输入模式')" :help="s('The saved prompt is delivered to this branch as one input, in the chosen mode.', '保存的指令会以所选模式作为一条输入投递到该分支。')">
                    <USelectMenu v-model="draft.input_mode" value-key="value" :search-input="false" :aria-label="s('Input mode', '输入模式')" :items="inputModeItems" />
                  </UFormField>
                </template>
              </div>
            </template>
          </UCollapsible>
          <p class="settings-help">{{ s('Saving applies the schedule and the target to future runs. Accepted runs are unchanged.', '保存后，后续运行按新的计划与目标执行；已接收运行不变。') }}</p>
          <footer class="settings-actions schedule-footer">
            <UButton color="neutral" variant="soft" :to="listLocation">{{ s('Cancel', '取消') }}</UButton>
            <UButton type="submit" color="primary" :disabled="!dirty || !planValid || !targetValid || conflict || promptBytes > 32768 || !permitted('schedule.save')">{{ isNew ? draft.enabled ? s('Create and enable', '创建并启用') : s('Create paused task', '创建为暂停') : s('Save changes', '保存更改') }}</UButton>
          </footer>
        </fieldset>
      </form>
      <template v-else-if="selected">
        <div class="settings-list-toolbar">
          <div class="settings-actions">
            <UButton :color="tab === 'overview' ? 'primary' : 'neutral'" :variant="tab === 'overview' ? 'soft' : 'ghost'" :aria-pressed="tab === 'overview'" @click="tab = 'overview'">{{ s('Overview', '概览') }}</UButton>
            <UButton :color="tab === 'history' ? 'primary' : 'neutral'" :variant="tab === 'history' ? 'soft' : 'ghost'" :aria-pressed="tab === 'history'" :disabled="!connection.supports('schedule.history')" @click="loadHistory()">{{ s('Run history', '运行历史') }}</UButton>
          </div>
          <UButton color="neutral" variant="ghost" :disabled="loading || !online" @click="load">{{ s('Refresh', '刷新') }}</UButton>
        </div>
        <template v-if="tab === 'overview'">
          <section class="settings-card">
            <p>{{ planLabel(selected.schedule) }}</p>
            <p>{{ selected.enabled ? s('Enabled', '已启用') : s('Paused', '已暂停') }} · {{ s('Next run: ', '下次执行：') }}{{ timestamp(selected.next_window) }}</p>
            <p v-if="selected.schedule_error" class="schedule-error">{{ selected.schedule_error }}</p>
            <p v-if="selected.disabled_reason" class="settings-meta">{{ s('Disabled', '已停用') }}: {{ selected.disabled_reason.code }} · {{ timestamp(selected.disabled_reason.at) }}</p>
            <p><code>{{ targetLabel(selected.target) }}</code></p>
            <pre class="schedule-prompt">{{ selected.prompt }}</pre>
            <p>{{ s('Origin', '来源') }}: {{ selected.created_by === 'user' ? s('User', '用户') : s('Tool', '工具') }}</p>
            <UCollapsible>
              <UButton size="xs" color="neutral" variant="ghost" :label="s('Saved target and origin', '已保存的目标和来源')" />
              <template #content><pre class="schedule-json">{{ JSON.stringify({ target: selected.target, origin: selected.created_by, disabled_reason: selected.disabled_reason }, null, 2) }}</pre></template>
            </UCollapsible>
          </section>
          <div class="settings-actions">
            <UButton v-if="connection.supports('schedule.save')" color="primary" :to="{ name: 'settings-scheduled-edit', params: { jobId: id }, query: listQuery }">{{ s('Edit task', '编辑任务') }}</UButton>
            <UButton color="neutral" variant="soft" :disabled="!permitted('schedule.set_enabled')" @click="act(selected, 'set_enabled')">{{ selected.enabled ? s('Pause', '暂停') : s('Resume', '恢复') }}</UButton>
            <UButton color="neutral" variant="soft" :disabled="!permitted('schedule.run_now')" @click="act(selected, 'run_now')">{{ s('Run now', '立即运行') }}</UButton>
            <UButton color="error" variant="ghost" :disabled="!permitted('schedule.delete')" @click="act(selected, 'delete')">{{ s('Delete task', '删除任务') }}</UButton>
          </div>
        </template>
        <section v-else class="schedule-history">
          <p class="settings-help">{{ s('Recent runs, newest first. Times shown in ', '最近运行，新到旧。展示时区：') }}{{ Intl.DateTimeFormat().resolvedOptions().timeZone }}</p>
          <p v-if="historyBusy" role="status">{{ s('Loading…', '加载中…') }}</p>
          <UAlert v-if="historyError" class="settings-banner" color="error" variant="soft" role="alert" :description="historyError" />
          <div v-for="run in runs" :key="run.run_id" class="schedule-job">
            <div class="schedule-job-heading"><span>{{ runState(run.state) }}</span><time :datetime="run.created_at" class="settings-meta">{{ timestamp(run.created_at) }}</time></div>
            <p v-if="run.detail">{{ run.detail }}</p>
            <p v-if="run.ended_at" class="settings-meta">{{ s('Ended', '结束') }}: {{ timestamp(run.ended_at) }}</p>
            <UCollapsible :unmount-on-hide="false">
              <UButton size="xs" color="neutral" variant="ghost" :label="s('Run details', '运行详情')" />
              <template #content>
                <div class="schedule-run-details">
                  <p class="settings-meta">{{ run.run_id }}</p>
                  <pre v-if="run.policy">{{ JSON.stringify(run.policy, null, 2) }}</pre>
                  <UButton v-if="run.session_id" size="xs" variant="link" class="p-0" :to="{ name: 'chat-session', params: { sessionId: run.session_id } }">{{ s('Open session', '打开会话') }}</UButton>
                </div>
              </template>
            </UCollapsible>
          </div>
          <p v-if="runs && !runs.length" class="settings-empty">{{ s('No runs recorded.', '没有运行记录。') }}</p>
          <UButton color="neutral" variant="soft" class="schedule-more" :disabled="historyBusy || !online" @click="loadHistory(50)">{{ s('Read latest 50 runs', '读取最近 50 次运行') }}</UButton>
        </section>
      </template>
      <UAlert v-else-if="opened && jobs && !loading" class="settings-banner" color="neutral" variant="soft" role="status" :description="s('This task no longer exists. Return to the task list.', '任务已不存在，请返回列表。')" />
    </template>
  </article>
</template>

<style scoped>
.schedule-header, .schedule-job-heading { display: flex; justify-content: space-between; align-items: start; gap: var(--space-3); }
.schedule-clock { color: var(--muted-foreground); flex: none; margin-top: 4px; }
.schedule-back { margin-bottom: var(--space-4); padding-inline: 0; }
.schedule-filters, .schedule-run-meta { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--space-4); }
.schedule-filters { margin-bottom: var(--space-4); }
.schedule-list { border-top: 1px solid var(--border); }
.schedule-job { padding-block: var(--space-5); border-bottom: 1px solid var(--border); }
.schedule-job p { margin-block: var(--space-2); font-size: var(--text-md); overflow-wrap: anywhere; }
.schedule-error { color: var(--danger); font-size: var(--text-xs); }
.schedule-run-meta { margin-block: var(--space-3); font-size: var(--text-sm); color: var(--muted-foreground); }
.schedule-form { min-width: 0; margin: 0; padding: 0; border: 0; }
.schedule-deliver, .schedule-json, .schedule-run-details { margin-top: var(--space-3); }
.schedule-footer { margin-top: var(--space-5); justify-content: flex-end; }
.schedule-prompt, pre { white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; }
.schedule-prompt { padding-block: var(--space-4); border-block: 1px solid var(--border); }
.schedule-more { margin-top: var(--space-4); }
.settings-list-toolbar > :first-child { flex: 1 1 180px; min-width: 0; }
@media (max-width: 719px) { .schedule-filters, .schedule-run-meta { grid-template-columns: minmax(0, 1fr); } .schedule-page :is(button, a) { min-height: 44px; } }
</style>
