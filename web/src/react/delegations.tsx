import { Button, Modal, Popover } from "@heroui/react"
import { ArrowLeft, ChevronRight, GitFork } from "lucide-react"
import { useEffect, useState } from "react"
import { ROOT_BRANCH_ID, type Delegation } from "../rpc/protocol"
import { readBranches, readFinishedDelegations, readPendingDelegations } from "../rpc/read"
import { appLocale } from "./i18n"
import { connection, useStoreValue } from "./stores"

const readLocale = () => appLocale.value
const labels = {
  en: {
    menu: "Delegations", active: "Active", recent: "Recently settled", empty: "No delegations yet.",
    loading: "Loading delegations…", retry: "Retry", back: "Back", close: "Close",
    task: "Delegated task", member: "Open member conversation",
    starting: "Starting", running: "Running", completed: "Completed", failed: "Failed", cancelled: "Cancelled",
  },
  zh: {
    menu: "委派", active: "进行中", recent: "近期结束", empty: "暂无委派任务。",
    loading: "正在加载委派任务…", retry: "重试", back: "返回", close: "关闭",
    task: "委派任务", member: "打开成员会话",
    starting: "正在启动", running: "进行中", completed: "已完成", failed: "失败", cancelled: "已取消",
  },
}

type Task = Delegation & { branchId: string; line?: string }
type Inventory = { tasks: Task[]; errors: string[] }

async function readInventory(sessionId: string, cancelled: () => boolean): Promise<Inventory> {
  const { branches } = await connection.client.call("session.branches", { sessionId }, readBranches)
  const inventory: Inventory = { tasks: [], errors: [] }
  for (const { branchId, purpose } of branches) {
    if (cancelled()) break
    const params = { sessionId, branchId }
    const line = branchId === ROOT_BRANCH_ID ? undefined : purpose || branchId
    try {
      const first = await connection.client.call("session.collaboration.pending", params, readPendingDelegations)
      if (cancelled()) break
      const window = { ...params, through: first.through }
      const settled = await connection.client.call("session.collaboration.finished", window, readFinishedDelegations)
      inventory.tasks.push(...[...first.tasks, ...settled].map((task) => ({ ...task, branchId, line })))
      let after = first.next
      while (after != null && !cancelled()) {
        const page = await connection.client.call("session.collaboration.pending", { ...window, after }, readPendingDelegations)
        inventory.tasks.push(...page.tasks.map((task) => ({ ...task, branchId, line })))
        after = page.next
      }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause)
      inventory.errors.push(line === undefined ? message : `${line} · ${message}`)
    }
  }
  return inventory
}

export function DelegationsMenu({ sessionId, revision, openMember }: { sessionId: string; revision: number; openMember: (id: string, title: string) => void }) {
  const locale = useStoreValue(readLocale)
  const text = locale === "zh-CN" ? labels.zh : labels.en
  const [open, setOpen] = useState(false)
  function selectMember(id: string, title: string) {
    setOpen(false)
    openMember(id, title)
  }
  const narrow = matchMedia("(max-width: 719px)").matches
  const trigger = <Button size="sm" variant="tertiary"><GitFork size={15} aria-hidden="true" /> {text.menu}</Button>
  if (narrow) return <Modal isOpen={open} onOpenChange={setOpen}>
    {trigger}
    <Modal.Backdrop>
      <Modal.Container placement="bottom">
        <Modal.Dialog className="rw-delegation-dialog" aria-label={text.menu}>
          <Modal.CloseTrigger aria-label={text.close} />
          {open && <DelegationList sessionId={sessionId} revision={revision} text={text} openMember={selectMember} />}
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  </Modal>

  return <Popover isOpen={open} onOpenChange={setOpen}>
    {trigger}
    <Popover.Content placement="bottom end" className="rw-delegation-dialog">
      <Popover.Dialog aria-label={text.menu}>
        {open && <DelegationList sessionId={sessionId} revision={revision} text={text} openMember={selectMember} />}
      </Popover.Dialog>
    </Popover.Content>
  </Popover>
}

function DelegationList({ sessionId, revision, text, openMember }: { sessionId: string; revision: number; text: typeof labels.en; openMember: (id: string, title: string) => void }) {
  const [inventory, setInventory] = useState<Inventory>()
  const [retry, setRetry] = useState(0)
  const [selected, setSelected] = useState<Task>()
  useEffect(() => {
    let cancelled = false
    const timer = window.setTimeout(() => {
      void readInventory(sessionId, () => cancelled).then((value) => {
        if (!cancelled) setInventory(value)
      }, (cause: unknown) => {
        if (!cancelled) setInventory({ tasks: [], errors: [cause instanceof Error ? cause.message : String(cause)] })
      })
    }, 100)
    return () => { cancelled = true; window.clearTimeout(timer) }
  }, [sessionId, revision, retry])

  const current = selected && (inventory?.tasks.find(({ id, branchId }) => id === selected.id && branchId === selected.branchId) ?? selected)
  const memberId = current?.memberId
  if (current) return <section className="rw-delegation-list">
    <Button autoFocus size="sm" variant="tertiary" onPress={() => setSelected(undefined)}><ArrowLeft size={14} aria-hidden="true" /> {text.back}</Button>
    <h2>{current.title || text.task}</h2>
    <p className="rw-muted">{[current.line, current.agentId, text[current.state]].filter(Boolean).join(" · ")}</p>
    {current.prompt && <p className="rw-delegation-text">{current.prompt}</p>}
    {current.reason && <p className="rw-error">{current.reason}</p>}
    {current.result && <p className="rw-delegation-text">{current.result}</p>}
    {memberId && <Button size="sm" variant="secondary" onPress={() => openMember(memberId, current.title || text.task)}>
      {text.member}
    </Button>}
  </section>

  const tasks = inventory?.tasks ?? []
  const active = tasks.filter(({ state }) => state === "running" || state === "starting").sort((a, b) => a.at - b.at)
  const recent = tasks.filter(({ state }) => state !== "running" && state !== "starting").sort((a, b) => b.at - a.at)

  return <section className="rw-delegation-list">
    <h2>{text.menu}</h2>
    {inventory === undefined && <p role="status">{text.loading}</p>}
    {inventory?.errors.map((error, index) => <p key={index} className="rw-error" role="alert">{error}</p>)}
    {Boolean(inventory?.errors.length) && <Button size="sm" variant="tertiary" onPress={() => setRetry((value) => value + 1)}>{text.retry}</Button>}
    {inventory && !inventory.errors.length && !tasks.length && <p className="rw-muted">{text.empty}</p>}
    {[{ title: text.active, group: active }, { title: text.recent, group: recent }].map(({ title, group }) => group.length > 0 && <section key={title}>
      <h3>{title}</h3>
      <ul>{group.map((task) => <li key={`${task.branchId}:${task.id}`}>
        <button className="rw-delegation-row" type="button" onClick={() => setSelected(task)}>
          <span>{task.title || text.task}{task.line && <small>{task.line}</small>}</span>
          <small>{text[task.state]}</small><ChevronRight size={14} aria-hidden="true" />
        </button>
      </li>)}</ul>
    </section>)}
  </section>
}
