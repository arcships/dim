import { useState } from "react"
import type { Schedule } from "../rpc/scheduler"
import { FieldSelect } from "./field-select"

export type ScheduleText = (en: string, zh: string) => string
export const browserTimeZone = Intl.DateTimeFormat().resolvedOptions().timeZone
const maximum = 4_294_967_295

export function ScheduleSelect<T extends string>({ label, value, options, onChange }: {
  label: string; value: T; options: Array<{ id: T; label: string }>; onChange: (value: T) => void
}) {
  return <div className="rw-select-field">
    <span>{label}</span>
    <FieldSelect label={label} value={value} options={options.map(({ id, label }) => ({ value: id, label }))} onChange={onChange} />
  </div>
}

type Plan = {
  kind: "once" | "daily" | "weekly" | "interval" | "cron"
  time: string; once: string; instant: string; days: number[]; minutes: string; cron: string; timezone: string
}

function localMinute(date: Date) {
  const pad = (part: number) => String(part).padStart(2, "0")
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

function readPlan(initial: Schedule): Plan {
  const simple = "cron" in initial ? /^(\d{1,2}) (\d{1,2}) \* \* (\*|[0-6](?:,[0-6])*)$/.exec(initial.cron) : null
  const days = simple?.[3] === "*" ? undefined : simple?.[3]?.split(",").map(Number)
  return {
    kind: "at" in initial ? "once" : "every_minutes" in initial ? "interval" : days ? "weekly" : simple ? "daily" : "cron",
    time: simple ? `${simple[2]!.padStart(2, "0")}:${simple[1]!.padStart(2, "0")}` : "09:00",
    once: "at" in initial ? localMinute(new Date(initial.at)) : "",
    instant: "",
    days: days ?? [1, 2, 3, 4, 5],
    minutes: "every_minutes" in initial ? String(initial.every_minutes) : "60",
    cron: "cron" in initial ? initial.cron : "0 9 * * 1-5",
    timezone: "cron" in initial ? initial.tz ?? "" : browserTimeZone,
  }
}

export function SchedulePlan({ initial, text, onChange }: {
  initial: Schedule; text: ScheduleText; onChange: (schedule: Schedule | undefined) => void
}) {
  const [plan, setPlan] = useState(() => readPlan(initial))
  const [error, setError] = useState("")
  const [ambiguous, setAmbiguous] = useState(false)
  function change(patch: Partial<Plan>) {
    const next = { ...plan, ...patch }
    setPlan(next)
    setError("")
    setAmbiguous(false)
    try {
      if (next.kind === "interval") {
        const every_minutes = Number(next.minutes)
        if (!Number.isInteger(every_minutes) || every_minutes < 1 || every_minutes > maximum) {
          throw new Error(text("Enter a whole interval of at least one minute.", "请输入至少一分钟的整数间隔。"))
        }
        onChange({ every_minutes })
      } else if (next.kind === "once") {
        const date = new Date(next.instant || next.once)
        if (!next.instant) {
          if (!Number.isFinite(date.getTime()) || localMinute(date) !== next.once) {
            throw new Error(text("Choose an existing local minute.", "请选择有效的本地分钟。"))
          }
          for (let offset = -180; offset <= 180; offset++) {
            if (offset !== 0 && localMinute(new Date(date.getTime() + offset * 60_000)) === next.once) {
              setAmbiguous(true)
              throw new Error(text("This local minute occurs twice. Specify its UTC offset.", "此本地分钟出现两次，请指定 UTC 偏移。"))
            }
          }
        } else {
          setAmbiguous(true)
          if (!/(Z|[+-]\d{2}:\d{2})$/.test(next.instant)) throw new Error(text("Include Z or an explicit UTC offset.", "请输入 Z 或明确的 UTC 偏移。"))
        }
        if (!Number.isFinite(date.getTime()) || date.getSeconds() !== 0 || date.getMilliseconds() !== 0) {
          throw new Error(text("Choose a valid time at minute precision.", "请选择精确到分钟的有效时间。"))
        }
        onChange({ at: date.toISOString() })
      } else {
        const [hour, minute] = next.time.split(":").map(Number)
        const weekdays = next.kind === "daily" ? "*" : [...next.days].sort((a, b) => a - b).join(",")
        if (next.kind !== "cron" && (!/^([01]\d|2[0-3]):[0-5]\d$/.test(next.time) || !weekdays)) {
          throw new Error(text("Choose a time and at least one weekday.", "请选择时间和至少一个星期几。"))
        }
        const cron = next.kind === "cron" ? next.cron.trim() : `${minute} ${hour} * * ${weekdays}`
        if (cron.split(/\s+/).length !== 5) throw new Error(text("Cron needs five fields.", "Cron 需要五个字段。"))
        const timezone = next.timezone.trim()
        if (timezone) new Intl.DateTimeFormat("en", { timeZone: timezone })
        onChange({ cron, ...(timezone ? { tz: timezone } : {}) })
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
      onChange(undefined)
    }
  }

  const advanced = plan.kind === "interval" || plan.kind === "cron"
  return <section className="rw-settings-card rw-settings-fields rw-schedule-fields">
    <h2>{text("When to run", "运行计划")}</h2>
    <ScheduleSelect<Plan["kind"]> label={text("Schedule type", "计划类型")} value={plan.kind}
      options={[
        { id: "once", label: text("Once", "仅一次") },
        { id: "daily", label: text("Every day", "每天") },
        { id: "weekly", label: text("Every week", "每周") },
        { id: "interval", label: text("Advanced · Every N minutes", "高级 · 每隔 N 分钟") },
        { id: "cron", label: text("Advanced · Custom Cron", "高级 · 自定义 Cron") },
      ]} onChange={(kind) => change({ kind, instant: "" })} />
    {plan.kind === "once" && <>
      <label>{text("Date and time", "运行日期和时间")}
        <input type="datetime-local" value={plan.once} onChange={(event) => change({ once: event.target.value, instant: "" })} />
      </label>
      <p>{text("Browser time zone", "浏览器时区")} · {browserTimeZone}</p>
      {ambiguous && <label>{text("Exact instant with UTC offset", "带 UTC 偏移的明确时间")}
        <input value={plan.instant} placeholder="2026-11-01T01:30:00-05:00" onChange={(event) => change({ instant: event.target.value })} />
      </label>}
    </>}
    {!advanced && plan.kind !== "once" && <label>{text("Run time", "运行时间")}
      <input type="time" value={plan.time} onChange={(event) => change({ time: event.target.value })} />
    </label>}
    {plan.kind === "weekly" && <div className="rw-setting-options">
      {[text("Sun", "周日"), text("Mon", "周一"), text("Tue", "周二"), text("Wed", "周三"), text("Thu", "周四"), text("Fri", "周五"), text("Sat", "周六")].map((label, day) =>
        <label className="rw-recording-toggle" key={day}><input type="checkbox" checked={plan.days.includes(day)} onChange={(event) => {
          change({ days: event.target.checked ? [...plan.days, day] : plan.days.filter((value) => value !== day) })
        }} />{label}</label>)}
    </div>}
    {plan.kind === "interval" && <>
      <label>{text("Interval in minutes", "间隔分钟数")}
        <input type="number" min="1" max={maximum} value={plan.minutes} onChange={(event) => change({ minutes: event.target.value })} />
      </label>
      <p>{text("The interval starts when the host enables or resumes the task.", "间隔从主机启用或恢复任务时开始计算。")}</p>
    </>}
    {plan.kind === "cron" && <label>{text("Cron · minute hour day month weekday", "Cron · 分 时 日 月 周")}
      <input value={plan.cron} onChange={(event) => change({ cron: event.target.value })} />
    </label>}
    {(plan.kind === "cron" || plan.kind === "daily" || plan.kind === "weekly") && <>
      <label>{text("Execution time zone", "执行时区")}
        <input value={plan.timezone} placeholder={browserTimeZone} onChange={(event) => change({ timezone: event.target.value })} />
      </label>
      <p>{text("Blank uses the CLI host’s time zone.", "留空使用 CLI 主机时区。")}</p>
    </>}
    {error && <p role="alert" className="rw-error">{error}</p>}
  </section>
}
