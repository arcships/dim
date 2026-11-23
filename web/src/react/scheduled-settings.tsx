import { Button, ComboBox, Input, Label, ListBox } from "@heroui/react"
import { useEffect, useLayoutEffect, useState, type FormEvent } from "react"
import { RpcError } from "../rpc/client"
import { ROOT_BRANCH_ID, type PermissionPreset } from "../rpc/protocol"
import { readScheduledDelete, readScheduledHistory, readScheduledJob, readScheduledJobs, readScheduledRun, readScheduledSave,
  type InputMode, type JobInput, type Schedule, type ScheduledJob, type ScheduledRun, type Target } from "../rpc/scheduler"
import { useUiStore } from "../stores/ui"
import { browserTimeZone, SchedulePlan, ScheduleSelect, type ScheduleText } from "./schedule-plan"
import { connection, sessions, useStoreValue } from "./stores"

export type SchedulePage = { kind: "list" } | { kind: "new" } | { kind: "job"; id: string; edit: boolean }
const routeBase = "/settings/scheduled"
const readOnline = () => connection.phase === "connected"
const readWorkspaces = () => sessions.workspaces
const readLastCwd = () => sessions.byId.get(useUiStore().lastSessionId ?? "")?.cwd
const rejectedWrites = ["schedule_conflict", "schedule_invalid", "schedule_not_found", "unknown_method", "service_reloading", "service_unavailable", "invalid_params"]
const timestamp = (value?: string) => value ? new Date(value).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }) : "—"
const workspaceOf = (target: Target) => target.kind === "launch" ? target.cwd : ""
const errorMessage = (cause: unknown) => cause instanceof Error ? cause.message : String(cause)

function scheduleLabel(schedule: Schedule, text: ScheduleText) {
  if ("every_minutes" in schedule) return text(`Every ${schedule.every_minutes} minutes`, `每隔 ${schedule.every_minutes} 分钟`)
  if ("at" in schedule) return `${text("Once", "仅一次")} · ${timestamp(schedule.at)}`
  const daily = /^(\d{1,2}) (\d{1,2}) \* \* \*$/.exec(schedule.cron)
  const label = daily
    ? `${text("Every day", "每天")} ${daily[2]!.padStart(2, "0")}:${daily[1]!.padStart(2, "0")}`
    : `Cron ${schedule.cron}`
  return `${label} · ${schedule.tz ?? text("Host time zone", "主机时区")}`
}

function targetLabel(target: Target, text: ScheduleText) {
  return target.kind === "launch" ? `${text("New session in", "新建会话工作区")} ${target.cwd}`
    : `${text("Deliver to", "投递到")} ${target.session_id} / ${target.branch_id} (${target.input_mode})`
}

export function ScheduledSettings({ page, text, open, openSession, onDirty }: {
  page: SchedulePage
  text: ScheduleText
  open: (path: string, replace?: boolean, saved?: boolean) => void
  openSession: (id: string) => void
  onDirty: (dirty: boolean) => void
}) {
  const online = useStoreValue(readOnline)
  const knownWorkspaces = useStoreValue(readWorkspaces)
  const lastCwd = useStoreValue(readLastCwd)
  const [filters, setFilters] = useState(() => {
    const params = new URLSearchParams(location.search)
    return { q: params.get("q") ?? "", state: params.get("state") ?? "all", workspace: params.get("workspace") ?? "" }
  })
  const [jobs, setJobs] = useState<ScheduledJob[]>()
  const [runs, setRuns] = useState<ScheduledRun[]>()
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [conflict, setConflict] = useState(false)
  const [uncertain, setUncertain] = useState(false)
  const [error, setError] = useState("")
  const [notice, setNotice] = useState("")
  const [historyError, setHistoryError] = useState("")
  const [historyBusy, setHistoryBusy] = useState(false)
  const [editorVersion, setEditorVersion] = useState(0)
  const [editorDirty, setEditorDirty] = useState(false)
  const routeCwd = new URLSearchParams(location.search).get("cwd")
  const id = page.kind === "job" ? page.id : undefined
  const editing = page.kind === "new" || (page.kind === "job" && page.edit)
  const selected = jobs?.find((job) => job.id === id)
  const supported = online && connection.supports("schedule.list")
  const targetWorkspaces = [...new Set(jobs?.flatMap(({ target }) => target.kind === "launch" ? [target.cwd] : []) ?? [])].sort()
  const workspaceOptions = [...new Set([...knownWorkspaces.map(({ cwd }) => cwd), ...targetWorkspaces])]
  const jobPath = (jobId: string) => `/${encodeURIComponent(jobId)}`
  const path = (suffix = "") => {
    const query = new URLSearchParams()
    if (filters.q) query.set("q", filters.q)
    if (filters.state !== "all") query.set("state", filters.state)
    if (filters.workspace) query.set("workspace", filters.workspace)
    if (routeCwd) query.set("cwd", routeCwd)
    return `${routeBase}${suffix}${query.size ? `?${query}` : ""}`
  }
  const permitted = (method: string) => online && !busy && !loading && !uncertain && connection.supports(method)

  function acceptDirty(value: boolean) {
    setEditorDirty(value)
    onDirty(value)
  }

  async function history(jobId: string, limit: number) {
    if (!online || !connection.supports("schedule.history")) return
    setHistoryBusy(true)
    setHistoryError("")
    try {
      setRuns(await connection.client.call("schedule.history", { job_id: jobId, limit }, readScheduledHistory))
    } catch (cause) {
      setHistoryError(errorMessage(cause))
    } finally {
      setHistoryBusy(false)
    }
  }

  async function load(manual = false) {
    if (!supported || loading || busy) return
    if (editorDirty && (!manual || !window.confirm(text("Discard the draft and reload this task?", "放弃草稿并重新加载此任务？")))) return
    setLoading(true)
    setError("")
    try {
      setJobs(await connection.client.call("schedule.list", {}, readScheduledJobs))
      if (runs !== undefined && id) await history(id, 20)
      if (editing && page.kind !== "new") setEditorVersion((version) => version + 1)
      setConflict(false)
      if (manual && page.kind === "list") setUncertain(false)
    } catch (cause) {
      setError(errorMessage(cause))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    if (supported) void load()
  }, [supported])

  useEffect(() => {
    setRuns(undefined)
    setHistoryError("")
    setConflict(false)
    setError("")
  }, [id, editing])

  function writeFailed(cause: unknown) {
    setConflict(cause instanceof RpcError && cause.appError === "schedule_conflict")
    setUncertain(!(cause instanceof RpcError) || !rejectedWrites.includes(cause.appError ?? ""))
    setError(errorMessage(cause))
  }

  async function save(input: JobInput, baseline: JobInput) {
    if (!permitted("schedule.save") || conflict || (page.kind !== "new" && !selected)) return
    if (input.target.kind === "launch" && input.target.policy === "full-access" && (
      page.kind === "new" || JSON.stringify(input.target) !== JSON.stringify(baseline.target)
    ) && !window.confirm(text("Save a task that can request full access on the host?", "保存可请求主机完全访问权限的任务？"))) return
    const patch: Partial<JobInput> & { id?: string } = selected ? { id: selected.id } : {}
    for (const key of ["name", "prompt", "schedule", "target", "enabled"] as const) {
      if (!selected || JSON.stringify(input[key]) !== JSON.stringify(baseline[key])) Object.assign(patch, { [key]: input[key] })
    }
    setBusy(true)
    setError("")
    try {
      const result = await connection.client.call("schedule.save", { job: patch, ...(selected ? { revision: selected.revision } : {}) }, readScheduledSave)
      setJobs((list) => [...(list?.filter(({ id }) => id !== result.job.id) ?? []), result.job])
      setNotice(text("Task saved.", "任务已保存。"))
      open(path(jobPath(result.job.id)), true, true)
    } catch (cause) {
      writeFailed(cause)
    } finally {
      setBusy(false)
    }
  }

  async function act(job: ScheduledJob, action: "set_enabled" | "run_now" | "delete") {
    const method = `schedule.${action}`
    if (!permitted(method)) return
    if (action === "run_now" && !window.confirm(text(
      `Run “${job.name}” now? ${targetLabel(job.target, text)}. The task keeps its current enablement.`,
      `立即运行「${job.name}」？${targetLabel(job.target, text)}。任务保留当前启用状态。`,
    ))) return
    if (action === "delete" && !window.confirm(text(`Delete “${job.name}”? Accepted runs continue.`, `删除「${job.name}」？已接收的运行将继续。`))) return
    if (action === "set_enabled" && !job.enabled && job.target.kind === "launch" && job.target.policy === "full-access"
      && !window.confirm(text("Resume this task with its full-access request?", "恢复此任务并保留完全访问权限请求？"))) return
    setBusy(true)
    setError("")
    setNotice("")
    try {
      if (action === "run_now") {
        const result = await connection.client.call(method, { job_id: job.id }, readScheduledRun)
        setNotice([result.state, result.detail].filter(Boolean).join(" · "))
        try {
          setJobs(await connection.client.call("schedule.list", {}, readScheduledJobs))
        } catch (cause) {
          setError(errorMessage(cause))
        }
        if (runs !== undefined && id) await history(id, 20)
      } else if (action === "set_enabled") {
        const updated = await connection.client.call(method, { job_id: job.id, enabled: !job.enabled }, readScheduledJob)
        setJobs((list) => list?.map((item) => item.id === updated.id ? updated : item))
        setNotice(text("Task enablement updated.", "任务启用状态已更新。"))
      } else {
        await connection.client.call(method, { job_id: job.id }, readScheduledDelete)
        setJobs((list) => list?.filter((item) => item.id !== job.id))
        setNotice(text("Task deleted. Accepted runs continue.", "任务已删除，已接收的运行将继续。"))
        open(path(), true)
      }
    } catch (cause) {
      writeFailed(cause)
    } finally {
      setBusy(false)
    }
  }

  function controls(job: ScheduledJob, detail = false) {
    return <div className="rw-setting-options">
      {detail && <Button size="sm" variant="secondary" isDisabled={!permitted("schedule.save")} onPress={() => open(path(`${jobPath(job.id)}/edit`))}>{text("Edit task", "编辑任务")}</Button>}
      <Button size="sm" variant="secondary" isDisabled={!permitted("schedule.set_enabled")} onPress={() => void act(job, "set_enabled")}>
        {job.enabled ? text("Pause", "暂停") : text("Resume", "恢复")}
      </Button>
      <Button size="sm" variant="secondary" isDisabled={!permitted("schedule.run_now")} onPress={() => void act(job, "run_now")}>{text("Run now", "立即运行")}</Button>
      {detail && <Button size="sm" variant="danger-soft" isDisabled={!permitted("schedule.delete")} onPress={() => void act(job, "delete")}>{text("Delete task", "删除任务")}</Button>}
    </div>
  }

  const filtered = jobs?.filter((job) => {
    const workspace = workspaceOf(job.target)
    return `${job.name} ${workspace}`.toLowerCase().includes(filters.q.trim().toLowerCase())
      && (!filters.workspace || workspace === filters.workspace)
      && (filters.state === "all" || (filters.state === "invalid" ? job.schedule_error !== undefined : job.enabled === (filters.state === "enabled")))
  })

  return <article className="rw-settings rw-scheduled">
    {page.kind !== "list" && <Button size="sm" variant="tertiary" className="rw-settings-reload" onPress={() => open(path())}>{text("Back to scheduled tasks", "返回定时任务")}</Button>}
    <h1>{page.kind === "new" ? text("New scheduled task", "新建定时任务") : editing ? text("Edit scheduled task", "编辑定时任务") : selected?.name ?? text("Scheduled tasks", "定时任务")}</h1>
    <p className="rw-muted">{text("The CLI host runs tasks while it is running, even after you close the browser.", "CLI 主机运行时会调度任务，关闭浏览器不影响调度。")}</p>
    {!online && <p role="status">{text("Disconnected. Shown data may be stale.", "连接已断开，所示数据可能已过期。")}</p>}
    {online && !supported && <p role="status">{text("Scheduled tasks are unavailable through this connection.", "当前连接无法使用定时任务。")}</p>}
    {notice && <p role="status">{notice}</p>}
    {error && <p role="alert" className="rw-error">{error}</p>}
    {conflict && <p className="rw-muted">{text("The task changed elsewhere. Reload its current revision before saving. Your draft is still here.", "任务在别处已更新。请重新加载当前版本后再保存，草稿已保留。")}</p>}
    {uncertain && <p role="alert" className="rw-error">{text("The write outcome is unknown. Check the task list and run history before attempting it again.", "写入结果未知。重试前请检查任务列表和运行历史。")}</p>}
    {(supported || jobs) && <>
      <div className="rw-setting-options">
        <Button size="sm" variant="secondary" isDisabled={!supported || busy || loading} onPress={() => void load(true)}>{text("Refresh", "刷新")}</Button>
        {page.kind === "list" && connection.supports("schedule.save") && <Button size="sm" isDisabled={!permitted("schedule.save")} onPress={() => open(path("/new"))}>{text("New task", "新建任务")}</Button>}
      </div>
      {loading && <p role="status">{text("Loading tasks…", "正在加载任务…")}</p>}
      {page.kind === "list" && <>
        <div className="rw-settings-card rw-settings-fields rw-schedule-fields">
          <label>{text("Search name or workspace", "搜索名称或工作区")}
            <input type="search" value={filters.q} onChange={(event) => setFilters({ ...filters, q: event.target.value })} />
          </label>
          <ScheduleSelect label={text("Status", "状态")} value={filters.state} onChange={(state) => setFilters({ ...filters, state })}
            options={[
              { id: "all", label: text("All statuses", "全部状态") }, { id: "enabled", label: text("Enabled", "已启用") },
              { id: "paused", label: text("Paused", "已暂停") }, { id: "invalid", label: text("Invalid schedule", "计划无效") },
            ]} />
          <ScheduleSelect label={text("Workspace", "工作区")} value={filters.workspace || "all"} onChange={(workspace) => setFilters({ ...filters, workspace: workspace === "all" ? "" : workspace })}
            options={[{ id: "all", label: text("All workspaces", "全部工作区") }, ...targetWorkspaces.map((cwd) => ({ id: cwd, label: cwd }))]} />
        </div>
        {filtered?.map((job) => <section className="rw-settings-card" key={job.id}>
          <Button size="sm" variant="tertiary" onPress={() => open(path(jobPath(job.id)))}>{job.name}</Button>
          <JobSummary job={job} text={text} />
          <p>{text("Last result", "最近结果")} · {job.last_run ? `${job.last_run.state} · ${timestamp(job.last_run.ended_at ?? job.last_run.created_at)}` : text("Not run yet", "尚未运行")}</p>
          {controls(job)}
        </section>)}
        {filtered?.length === 0 && <p>{jobs?.length ? text("No matching tasks.", "没有匹配的任务。") : text("No scheduled tasks yet.", "还没有定时任务。")}</p>}
      </>}
      {editing && (page.kind === "new" || selected) && <ScheduledEditor key={`${id ?? "new"}:${editorVersion}`}
        job={selected} cwd={routeCwd ?? lastCwd ?? ""} workspaceOptions={workspaceOptions} text={text}
        disabled={!permitted("schedule.save") || conflict} onDirty={acceptDirty} save={save} cancel={() => open(path(id ? jobPath(id) : ""))} />}
      {!editing && selected && <>
        <div className="rw-setting-options">
          <Button size="sm" variant={runs === undefined ? "secondary" : "tertiary"} aria-pressed={runs === undefined} onPress={() => setRuns(undefined)}>{text("Overview", "概览")}</Button>
          <Button size="sm" variant={runs === undefined ? "tertiary" : "secondary"} aria-pressed={runs !== undefined}
            isDisabled={!online || historyBusy || !connection.supports("schedule.history")} onPress={() => {
              setRuns([])
              void history(selected.id, 20)
            }}>{text("Run history", "运行历史")}</Button>
        </div>
        {runs === undefined ? <>
          <section className="rw-settings-card">
            <JobSummary job={selected} text={text} />
            <pre>{selected.prompt}</pre>
            <p>{text("Origin", "来源")} · {selected.created_by === "user" ? text("User", "用户") : text("Tool", "工具")}</p>
            <details><summary>{text("Saved target and origin", "已保存的目标和来源")}</summary>
              <pre>{JSON.stringify({ target: selected.target, origin: selected.created_by, disabled_reason: selected.disabled_reason }, null, 2)}</pre>
            </details>
          </section>
          {controls(selected, true)}
        </> : <>
          {historyBusy && <p role="status">{text("Loading run history…", "正在加载运行历史…")}</p>}
          {historyError && <p role="alert" className="rw-error">{historyError}</p>}
          {!historyBusy && !historyError && runs.length === 0 && <p>{text("No runs recorded.", "没有运行记录。")}</p>}
          {runs.map((run) => <section className="rw-settings-card" key={run.run_id}>
            <h2>{run.state} · <time dateTime={run.created_at}>{timestamp(run.created_at)}</time></h2>
            {run.detail && <p>{run.detail}</p>}
            {run.ended_at && <p>{text("Ended", "结束")} · {timestamp(run.ended_at)}</p>}
            <details><summary>{text("Run details", "运行详情")}</summary>
              <code>{run.run_id}</code>
              {run.policy && <pre>{JSON.stringify(run.policy, null, 2)}</pre>}
            </details>
            {run.session_id && <Button size="sm" variant="secondary" onPress={() => openSession(run.session_id!)}>{text("Open session", "打开会话")}</Button>}
          </section>)}
          <Button size="sm" variant="secondary" className="rw-settings-reload" isDisabled={!online || historyBusy}
            onPress={() => void history(selected.id, 50)}>{text("Read latest 50 runs", "读取最近 50 次运行")}</Button>
        </>}
      </>}
      {page.kind === "job" && jobs && !selected && !loading && <p role="status">{text("This task no longer exists.", "此任务已不存在。")}</p>}
    </>}
  </article>
}

function JobSummary({ job, text }: { job: ScheduledJob; text: ScheduleText }) {
  return <>
    <p>{scheduleLabel(job.schedule, text)}</p>
    <p>{targetLabel(job.target, text)}</p>
    <p>{job.enabled ? text("Enabled", "已启用") : text("Paused", "已暂停")} · {text("Next run", "下次执行")} · {timestamp(job.next_window)}</p>
    {job.schedule_error && <p className="rw-error">{job.schedule_error}</p>}
    {job.disabled_reason && <p>{text("Disabled", "已停用")} · {job.disabled_reason.code} · {timestamp(job.disabled_reason.at)}</p>}
  </>
}

function ScheduledEditor({ job, cwd, workspaceOptions, text, disabled, save, cancel, onDirty }: {
  job?: ScheduledJob; cwd: string; workspaceOptions: string[]; text: ScheduleText; disabled: boolean
  save: (input: JobInput, baseline: JobInput) => Promise<void>; cancel: () => void; onDirty: (dirty: boolean) => void
}) {
  const launchTarget = (): Extract<Target, { kind: "launch" }> => ({ kind: "launch", cwd, max_run_minutes: 60 })
  const [baseline] = useState<JobInput>(() => job ?? { name: "", prompt: "", schedule: { cron: "0 9 * * *", tz: browserTimeZone }, target: launchTarget(), enabled: true })
  const [name, setName] = useState(baseline.name)
  const [prompt, setPrompt] = useState(baseline.prompt)
  const [enabled, setEnabled] = useState(baseline.enabled)
  const [schedule, setSchedule] = useState<Schedule | undefined>(baseline.schedule)
  const [target, setTarget] = useState<Target>(baseline.target)
  const normalized = target.kind === "session" ? { ...target, branch_id: target.branch_id || ROOT_BRANCH_ID } : target
  const input = schedule && { name, prompt, enabled, schedule, target: normalized }
  const dirty = input === undefined || (["name", "prompt", "enabled", "schedule", "target"] as const).some((key) => JSON.stringify(input[key]) !== JSON.stringify(baseline[key]))
  const promptBytes = new TextEncoder().encode(prompt).length
  const validTarget = target.kind === "launch" ? target.cwd.trim() !== "" && Number.isInteger(target.max_run_minutes)
    && target.max_run_minutes >= 1 && target.max_run_minutes <= 4_294_967_295 : target.session_id.trim() !== ""
  useLayoutEffect(() => {
    onDirty(dirty)
    return () => onDirty(false)
  }, [dirty])

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (input && dirty && !disabled && name.trim() && validTarget && promptBytes <= 32768) void save(input, baseline)
  }

  return <form onSubmit={submit}>
    <fieldset disabled={disabled} className="rw-settings-form rw-scheduled-editor">
      <section className="rw-settings-card rw-settings-fields rw-schedule-fields">
        <label>{text("Name", "名称")}<input value={name} onChange={(event) => setName(event.target.value)} required /></label>
        <label>{text("Instructions to run automatically", "未来自动执行的指令")}
          <textarea rows={6} value={prompt} onChange={(event) => setPrompt(event.target.value)} />
        </label>
        <small>{promptBytes} / 32768 {text("UTF-8 bytes", "UTF-8 字节")}</small>
        {!job && <label className="rw-recording-toggle">
          <input type="checkbox" checked={enabled} onChange={(event) => setEnabled(event.target.checked)} /> {text("Enable after creation", "创建后启用")}
        </label>}
      </section>
      <SchedulePlan initial={baseline.schedule} text={text} onChange={setSchedule} />
      <section className="rw-settings-card rw-settings-fields rw-schedule-fields">
        <h2>{text("Where it delivers", "投递到哪里")}</h2>
        <ScheduleSelect label={text("Target", "目标")} value={target.kind} options={[
          { id: "launch", label: text("Launch a new session", "新建会话") },
          { id: "session", label: text("Deliver to an existing session branch", "投递到已有会话分支") },
        ]} onChange={(kind) => {
          if (kind !== target.kind) setTarget(kind === "launch" ? launchTarget() : { kind, session_id: "", branch_id: ROOT_BRANCH_ID, input_mode: "queue" })
        }} />
        {target.kind === "launch" ? <>
          <ComboBox allowsCustomValue isRequired isDisabled={disabled} inputValue={target.cwd}
            defaultItems={workspaceOptions.map((path) => ({ id: path }))}
            onInputChange={(cwd) => setTarget({ ...target, cwd })}>
            <Label>{text("CLI host workspace", "CLI 主机工作区")}</Label>
            <ComboBox.InputGroup><Input /><ComboBox.Trigger /></ComboBox.InputGroup>
            <ComboBox.Popover className="rw-select-menu"><ListBox className="rw-select-list">{(item: { id: string }) => <ListBox.Item
              id={item.id} textValue={item.id} className="rw-select-option">
              <span>{item.id}</span><ListBox.ItemIndicator />
            </ListBox.Item>}</ListBox></ComboBox.Popover>
          </ComboBox>
          <ScheduleSelect<PermissionPreset | "default"> label={text("Requested permission", "声明权限")} value={target.policy ?? "default"}
            options={[
              { id: "default", label: text("Undeclared · Read only", "未声明 · 只读") }, { id: "read-only", label: text("Read only", "只读") },
              { id: "workspace-write", label: text("Workspace write", "工作区读写") }, { id: "full-access", label: text("Full access", "完全访问") },
            ]} onChange={(policy) => setTarget({ ...target, policy: policy === "default" ? undefined : policy })} />
          <label>{text("Maximum run time in minutes", "最长运行分钟数")}
            <input type="number" min="1" max="4294967295" value={Number.isFinite(target.max_run_minutes) ? target.max_run_minutes : ""}
              required onChange={(event) => setTarget({ ...target, max_run_minutes: event.target.valueAsNumber })} />
          </label>
          <label>{text("Agent profile ID (optional)", "代理配置 ID（可选）")}
            <input value={target.agent_id ?? ""} onChange={(event) => setTarget({ ...target, agent_id: event.target.value || undefined })} />
          </label>
        </> : <>
          <label>{text("Session ID", "会话 ID")}<input value={target.session_id} required onChange={(event) => setTarget({ ...target, session_id: event.target.value })} /></label>
          <label>{text("Branch", "分支")}<input value={target.branch_id} placeholder={ROOT_BRANCH_ID} onChange={(event) => setTarget({ ...target, branch_id: event.target.value })} /></label>
          <ScheduleSelect<InputMode> label={text("Input mode", "输入模式")} value={target.input_mode}
            options={[
              { id: "queue", label: text("Queue", "排队") }, { id: "steer", label: text("Steer", "插话") }, { id: "append", label: text("Append", "追加") },
            ]} onChange={(input_mode) => setTarget({ ...target, input_mode })} />
        </>}
      </section>
      <div className="rw-setting-options">
        <Button type="button" variant="secondary" onPress={cancel}>{text("Cancel", "取消")}</Button>
        <Button type="submit" isDisabled={!dirty || !input || !name.trim() || promptBytes > 32768 || !validTarget || disabled}>
          {job ? text("Save changes", "保存更改") : enabled ? text("Create and enable", "创建并启用") : text("Create paused task", "创建为暂停")}
        </Button>
      </div>
    </fieldset>
  </form>
}
