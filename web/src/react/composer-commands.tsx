import { useEffect, useRef } from "react"
import { goalCall, goalObjective, parseGoalRequest } from "../goal"
import { translate } from "./i18n"
import { intervalSeconds, loopReport, parseLoopRequest } from "../loop"
import { namesSkillToken, SKILL_TOKEN_PREFIX, skillCommandQuery } from "../skill-token"
import { chat } from "./stores"

export const commandOrder = [
  "new", "models", "compact", "usage", "effort", "permissions", "branch", "fork", "goal", "loop", "scheduled",
] as const
export type CommandName = typeof commandOrder[number]
export type SkillChoice = { name: string; description: string }
export type CommandChoice = { name: CommandName; skill?: false } | { name: string; skill: true; description: string }

export function takesWords(name: CommandName): boolean {
  return name === "branch" || name === "goal" || name === "loop" || name === "effort"
}

export function commandChoices(draft: string, available: ReadonlySet<CommandName>, skills: readonly SkillChoice[]): CommandChoice[] {
  if (!/^\/[a-z]*(?::[A-Za-z0-9_.-]*)?$/i.test(draft)) return []
  const { name, skillsOnly } = skillCommandQuery(draft)
  const commands = skillsOnly ? [] : commandOrder.filter((command) => available.has(command) && command.startsWith(name))
  return [
    ...commands.map((name) => ({ name } as const)),
    ...skills.filter((skill) => namesSkillToken(skill.name) && (skillsOnly || name !== "") && skill.name.toLowerCase().startsWith(name))
      .map((skill) => ({ ...skill, skill: true } as const)),
  ]
}

export function CommandPalette({ id, choices, selected, choose }: {
  id: string
  choices: readonly CommandChoice[]
  selected: number
  choose: (index: number) => void
}) {
  const active = useRef<HTMLLIElement>(null)
  useEffect(() => { active.current?.scrollIntoView({ block: "nearest" }) }, [selected])
  return <div className="rw-command-palette">
    <ul id={id} role="listbox" aria-label={translate("composer.commands")}>
      {choices.map((choice, index) => <li
        key={`${choice.skill ? "skill" : "command"}:${choice.name}`}
        id={`${id}-${index}`} role="option" aria-selected={selected === index} ref={selected === index ? active : undefined}
        onMouseDown={(event) => event.preventDefault()} onClick={() => choose(index)}>
        <code>{choice.skill ? SKILL_TOKEN_PREFIX : "/"}{choice.name}</code>
        <span>{choice.skill ? choice.description : translate(`composer.command.${choice.name}`)}</span>
      </li>)}
    </ul>
    <small>↑ ↓ · Enter · Tab · Esc</small>
  </div>
}

export async function contextCommand(name: "compact" | "usage", sessionId: string, branchId: string): Promise<string> {
  if (name === "compact") {
    const result = await chat.compact(sessionId, branchId)
    return result.status === "completed" ? translate("command.compacted") : `${translate("command.compactSkipped")} · ${result.reason}`
  }
  const reading = await chat.usage(sessionId, branchId)
  const input = Math.round(reading.estimatedInputTokens)
  const window = Math.round(reading.contextWindow)
  return translate("command.usage", {
    input: input.toLocaleString(),
    window: window.toLocaleString(),
    percent: window > 0 ? Math.floor((input * 100) / window) : 0,
    basis: reading.baselineSource === "provider_baseline" ? translate("command.basisProvider") : translate("command.basisEstimated"),
    output: Math.round(reading.outputTokens).toLocaleString(),
    threshold: Math.round(reading.thresholdTokens).toLocaleString(),
  })
}

export async function goalCommand(sessionId: string, branchId: string, words: string): Promise<string> {
  const request = parseGoalRequest(words)
  if (request === undefined) return translate("goal.usage")
  const current = await chat.readLineGoal(sessionId, branchId)
  const call = goalCall(request, current)
  if (call !== undefined) {
    await chat.writeGoal(sessionId, call, goalObjective(request), branchId)
    return translate("goal.sent", { action: translate(`goal.action.${call}`) })
  }
  if (current === undefined) return translate("goal.none")
  const { status, evaluator, objective } = current
  const label = translate(`goal.status.${status.kind}`, status.kind === "paused" ? { reason: status.reason.replaceAll("_", " ") } : undefined)
  return `${translate("goal.line", { status: label })}${evaluator ? ` · ${translate("goal.evaluator")}` : ""}\n${objective}`
}

export async function loopCommand(sessionId: string, branchId: string, words: string): Promise<string> {
  const request = parseLoopRequest(words)
  if (request === undefined) return translate("loop.usage")
  if (request.form === "show") return loopReport(await chat.readLoops(sessionId, branchId)).join("\n")
  if (request.form === "start") {
    const interval = request.interval === undefined ? undefined : intervalSeconds(request.interval)
    const id = await chat.startLoop(sessionId, request.prompt, interval, branchId)
    return translate("loop.started", { id })
  }
  let id = request.loopId
  if (id === undefined) {
    const live = (await chat.readLoops(sessionId, branchId)).filter(({ state }) => state !== "stopped" && state !== "expired")
    if (live.length !== 1) return translate("loop.noSingle")
    id = live[0]!.loopId
  }
  await chat.stopLoop(sessionId, id)
  return translate("loop.stopped", { id })
}
