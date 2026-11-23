import { Button, Popover } from "@heroui/react"
import { useVirtualizer } from "@tanstack/react-virtual"
import { ArrowDown, Check, ChevronRight, Copy, GitBranch, GitFork, List, Quote } from "lucide-react"
import { lazy, Suspense, type ReactNode, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react"
import { Streamdown } from "streamdown"
import { copyText } from "../clipboard"
import { markdownLinkPath } from "../features/chat/markdown-links"
import { resolveToolDisplay } from "../features/chat/tool-display"
import type { TranslationKey } from "../i18n/en"
import type { TimelineRow } from "../projection"
import { skillSegments } from "../skill-token"

const ToolDiff = lazy(() => import("./tool-diff"))

type ToolRow = Extract<TimelineRow, { kind: "tool" }>
type ReasoningRow = Extract<TimelineRow, { kind: "thinking" }>
type TranscriptItem = Exclude<TimelineRow, ToolRow | ReasoningRow>
export type UserMessage = Extract<TimelineRow, { kind: "user" }>
type TimelineItem = TranscriptItem | ReasoningRow | { kind: "execution"; key: string; steps: ToolRow[] }
type MessageActions = {
  quote?: (text: string) => void
  branch?: (row: UserMessage) => void
  fork?: (row: UserMessage) => void
}

function groupExecution(rows: readonly TimelineRow[]): TimelineItem[] {
  const items: TimelineItem[] = []
  for (const row of rows) {
    if (row.kind !== "tool") {
      items.push(row)
      continue
    }
    const previous = items.at(-1)
    if (previous?.kind === "execution") previous.steps.push(row)
    else items.push({ kind: "execution", key: `execution:${row.key}`, steps: [row] })
  }
  return items
}

type TimelineProps = {
  sessionId: string
  rows: readonly TimelineRow[]
  active: boolean
  slot: HTMLDivElement | null
  earlier: boolean
  loadingEarlier: boolean
  historyError: string
  loadEarlier: () => Promise<void>
  actions?: MessageActions
  children: ReactNode
}

export function Timeline({ sessionId, rows, active, slot, earlier, loadingEarlier, historyError, loadEarlier, actions, children }: TimelineProps) {
  const viewport = useRef<HTMLDivElement>(null)
  const historyBar = useRef<HTMLDivElement>(null)
  const [historyHeight, setHistoryHeight] = useState(0)
  const [following, setFollowing] = useState(true)
  const [newOutput, setNewOutput] = useState(false)
  const [historyVisible, setHistoryVisible] = useState(false)
  const [outlineOpen, setOutlineOpen] = useState(false)
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set())
  const items = useMemo(() => groupExecution(rows), [rows])
  const markers = useMemo(() => items.flatMap((row, index) => row.kind === "user" ? [{ row, index }] : []), [items])
  const showHistoryBar = earlier || historyError !== ""
  const jumping = useRef(false)
  const readInFlight = useRef(false)
  const lastRow = useRef(rows.at(-1))
  const loadHistory = useRef(loadEarlier)
  loadHistory.current = loadEarlier

  useLayoutEffect(() => {
    const element = historyBar.current
    if (element === null) {
      setHistoryHeight(0)
      return
    }
    const measure = () => setHistoryHeight(element.offsetHeight)
    const observer = new ResizeObserver(measure)
    measure()
    observer.observe(element)
    return () => observer.disconnect()
  }, [showHistoryBar])
  useEffect(() => {
    const root = viewport.current
    const sentinel = historyBar.current
    if (!active || !root || !sentinel) {
      setHistoryVisible(false)
      return
    }
    const observer = new IntersectionObserver(([entry]) => setHistoryVisible(entry?.isIntersecting === true), { root })
    observer.observe(sentinel)
    return () => observer.disconnect()
  }, [active, showHistoryBar])

  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => viewport.current,
    getItemKey: (index) => items[index]!.key,
    estimateSize: () => 76,
    anchorTo: "end",
    followOnAppend: following,
    scrollEndThreshold: following ? 96 : -1,
    scrollMargin: historyHeight,
    paddingStart: 12,
    paddingEnd: markers.length > 0 ? 44 : 12,
    overscan: 5,
  })
  const visibleItems = virtualizer.getVirtualItems()
  const initialized = useRef(false)
  useLayoutEffect(() => {
    if (items.length && !initialized.current) {
      initialized.current = true
      jumping.current = !virtualizer.isAtEnd(1)
      virtualizer.scrollToEnd()
    }
  }, [items.length, virtualizer])
  const followsEnd = useRef(following)
  followsEnd.current = following
  useLayoutEffect(() => {
    if (!active || slot === null) return
    const frame = requestAnimationFrame(() => {
      if (followsEnd.current) {
        jumping.current = !virtualizer.isAtEnd(1)
        virtualizer.scrollToEnd()
      }
    })
    return () => cancelAnimationFrame(frame)
  }, [active, slot, virtualizer])

  useLayoutEffect(() => {
    const previous = lastRow.current
    const last = rows.at(-1)
    lastRow.current = last
    if (!following && previous && last && last !== previous) setNewOutput(true)
  }, [rows, following])

  useEffect(() => {
    if (active && historyVisible && !following && earlier && !loadingEarlier && !historyError) void readHistory()
  }, [active, historyVisible, following, earlier, loadingEarlier, historyError])

  async function readHistory() {
    if (loadingEarlier || readInFlight.current) return
    setFollowing(false)
    jumping.current = false
    readInFlight.current = true
    try {
      await loadHistory.current()
    } finally {
      readInFlight.current = false
    }
  }

  function jumpToEnd() {
    setNewOutput(false)
    setFollowing(true)
    jumping.current = !virtualizer.isAtEnd(1)
    virtualizer.scrollToEnd({ behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" })
  }

  function toggle(key: string, open: boolean) {
    if (expanded.has(key) === open) return
    setFollowing(false)
    jumping.current = false
    setExpanded((previous) => {
      if (previous.has(key) === open) return previous
      const next = new Set(previous)
      if (open) next.add(key)
      else next.delete(key)
      return next
    })
  }

  return (
    <>
      <div className="rw-viewport" ref={viewport} tabIndex={0} aria-label="Conversation"
        onScroll={(event) => {
          if (!active) return
          const element = event.currentTarget
          const remaining = element.scrollHeight - element.clientHeight - element.scrollTop
          if (remaining <= 1) jumping.current = false
          if (remaining > 96 && !jumping.current) setFollowing(false)
        }}
        onWheel={() => { jumping.current = false }}
        onKeyDown={(event) => {
          if (event.target === event.currentTarget && ["PageUp", "ArrowUp", "Home", "PageDown", "ArrowDown", "End"].includes(event.key)) jumping.current = false
        }}>
        {showHistoryBar && (
          <div className="rw-history" ref={historyBar}>
            {historyError && <p role="alert">{historyError}</p>}
            <Button size="sm" variant="tertiary" isPending={loadingEarlier} onPress={() => void readHistory()}>
              {historyError ? "Retry loading history" : "Load earlier messages"}
            </Button>
          </div>
        )}
        <ol className="rw-timeline" style={{
          minHeight: virtualizer.getTotalSize(),
          paddingBlockStart: Math.max(0, (visibleItems[0]?.start ?? historyHeight) - historyHeight),
        }}>
          {visibleItems.map((entry) => {
            const item = items[entry.index]!
            return (
              <li className="rw-timeline-row" key={entry.key} data-index={entry.index} ref={virtualizer.measureElement}>
                {item.kind === "execution"
                  ? <Execution item={item} sessionId={sessionId} expanded={expanded} toggle={toggle} />
                  : item.kind === "thinking"
                    ? <Reasoning row={item} open={expanded.has(item.key)} onToggle={(open) => toggle(item.key, open)} />
                    : <TranscriptRow row={item} sessionId={sessionId} actions={actions} />}
              </li>
            )
          })}
        </ol>
      </div>
      <div className="rw-dock">
        {(!following || markers.length > 0) && <nav className="rw-timeline-controls" aria-label="Conversation navigation">
          {markers.length > 0 && <Popover isOpen={outlineOpen} onOpenChange={setOutlineOpen}>
            <Button className="rw-outline-trigger" size="sm" variant="tertiary" aria-label="Conversation outline">
              <List size={16} aria-hidden="true" /><span>Outline</span>
            </Button>
            <Popover.Content placement="top end" className="rw-outline">
              <Popover.Dialog aria-label="Conversation outline">
                <ol>
                  {markers.map(({ row, index }) => <li key={row.key}>
                    <button type="button" onClick={() => {
                      setOutlineOpen(false)
                      setFollowing(false)
                      jumping.current = false
                      virtualizer.scrollToIndex(index, { align: "start" })
                    }}>{row.text.trim() || "Attachment"}</button>
                  </li>)}
                </ol>
              </Popover.Dialog>
            </Popover.Content>
          </Popover>}
          {!following && <Button className="rw-latest-pill" size="sm" variant="secondary" onPress={jumpToEnd}>
            <ArrowDown size={15} aria-hidden="true" /><span>{newOutput ? "New messages · Return to latest" : "Return to latest"}</span>
          </Button>}
        </nav>}
        {children}
      </div>
    </>
  )
}

function Reasoning({ row, open, onToggle }: {
  row: ReasoningRow
  open: boolean
  onToggle: (open: boolean) => void
}) {
  const excerpt = row.text.trim().split(/\n/, 1)[0]?.replace(/^#+\s+|\*\*|__/g, "").trim()
  const seconds = row.elapsedMs === undefined ? undefined : Math.floor(row.elapsedMs / 1000)
  const time = seconds === undefined ? null : seconds === 0 ? "<1 second" : `${seconds} ${seconds === 1 ? "second" : "seconds"}`
  return <details className="rw-reasoning" open={open} onToggle={(event) => onToggle(event.currentTarget.open)}>
    <summary>
      <span className="rw-activity-label" title={excerpt}>{excerpt || "Reasoning"}</span>
      {(row.streaming || time) && <small>· {row.streaming ? "Thinking" : time}</small>}
      <ChevronRight size={14} />
    </summary>
    {open && <div className="rw-reasoning-body"><Markdown text={row.text} streaming={row.streaming} /></div>}
  </details>
}

const activityCounts = {
  search: (count: number) => `ran ${count} ${count === 1 ? "search" : "searches"}`,
  command: (count: number) => `ran ${count} ${count === 1 ? "command" : "commands"}`,
  read: (count: number) => `read ${count} ${count === 1 ? "file" : "files"}`,
  file: (count: number) => `made ${count} ${count === 1 ? "file change" : "file changes"}`,
  generic: (count: number) => `called ${count} ${count === 1 ? "tool" : "tools"}`,
}

const activityVerbs: Partial<Record<TranslationKey, string>> = {
  "tool.run": "Run",
  "tool.runBackground": "Run in background",
  "tool.read": "Read",
  "tool.edit": "Edit",
  "tool.write": "Write",
  "tool.listFiles": "List",
  "tool.glob": "Find",
  "tool.grep": "Grep",
  "tool.webSearch": "Search web",
  "tool.webFetch": "Fetch",
}

function describeTool(step: ToolRow) {
  const { renderer, labelKey } = resolveToolDisplay(step.name, step.input)
  const read = labelKey === "tool.read"
  const category = read ? "read" : renderer === "web" ? labelKey === "tool.webSearch" ? "search" : "generic" : renderer
  const { path, pattern } = step.input
  const field = renderer === "command" ? step.input.command
    : renderer === "search" ? labelKey === "tool.listFiles" ? path : [pattern, path].filter((part) => typeof part === "string" && part !== "").join(" · ")
      : renderer === "file" ? step.input.path
        : renderer === "web" ? step.input.query || step.input.url : undefined
  const target = typeof field === "string" ? field : ""
  const offset = typeof step.input.offset === "number" && step.input.offset > 0 ? step.input.offset : undefined
  const limit = typeof step.input.limit === "number" && step.input.limit > 0 ? step.input.limit : undefined
  const start = offset ?? 1
  const range = read && target
    ? limit !== undefined ? limit === 1 ? `L${start}` : `L${start}–${start + limit - 1}`
      : offset !== undefined ? `L${offset}+` : null
    : null
  const verb = labelKey === "tool.run" && !step.pending ? "Ran" : activityVerbs[labelKey] ?? step.name
  return { category, verb, target, range, command: renderer === "command" }
}

function Execution({ item, sessionId, expanded, toggle }: {
  item: Extract<TimelineItem, { kind: "execution" }>
  sessionId: string
  expanded: ReadonlySet<string>
  toggle: (key: string, open: boolean) => void
}) {
  const open = expanded.has(item.key)
  const descriptions = item.steps.map((step) => ({ step, ...describeTool(step) }))
  const summary = Object.entries(activityCounts).flatMap(([category, format]) => {
    const count = descriptions.filter((description) => description.category === category).length
    return count ? [format(count)] : []
  }).join(", ").replace(/^./, (letter) => letter.toUpperCase())
  const running = item.steps.some((step) => step.pending)
  const failed = item.steps.some((step) => step.result?.isError === true)
  return <details className="rw-execution" open={open} onToggle={(event) => toggle(item.key, event.currentTarget.open)}>
    <summary>
      <span className="rw-activity-label">{summary}</span>
      <ChevronRight size={15} />
      {running ? <small>Running</small> : failed && <small className="rw-error">Failed</small>}
    </summary>
    {open && <ol className="rw-execution-body">
      {descriptions.map(({ step, verb, target, range, command }) => <li key={step.key}>
        <details className="rw-tool" open={expanded.has(step.key)} onToggle={(event) => toggle(step.key, event.currentTarget.open)}>
          <summary>
            <span>{verb}</span>
            {target && (command
              ? <code className="rw-activity-label rw-tool-target" title={target}>{target}</code>
              : <span className="rw-activity-label rw-tool-target" title={target}>{target}</span>)}
            {range && <small>{range}</small>}
            {step.pending ? <small>Running</small> : step.result?.isError && <small className="rw-error">Failed</small>}
            <ChevronRight size={13} />
          </summary>
          {expanded.has(step.key) && <div className="rw-tool-output">
            {Object.keys(step.input).length > 0 && <pre>{JSON.stringify(step.input, null, 2)}</pre>}
            {step.result?.text && <pre>{step.result.text}</pre>}
            {step.result?.diff && <Suspense fallback={<pre>{step.result.diff}</pre>}><ToolDiff patch={step.result.diff} /></Suspense>}
            {step.result?.images?.map((image) => <Image key={image.source.fileId} sessionId={sessionId} fileId={image.source.fileId} />)}
          </div>}
        </details>
      </li>)}
    </ol>}
  </details>
}

function Image({ sessionId, fileId }: { sessionId: string; fileId: string }) {
  return <img className="rw-attachment" alt="Conversation attachment" loading="lazy" src={`/api/sessions/${encodeURIComponent(sessionId)}/files/${encodeURIComponent(fileId)}`} />
}

function Markdown({ text, streaming = false }: { text: string; streaming?: boolean }) {
  return <div className="rw-markdown" onClick={(event) => {
    if (!(event.target instanceof Element)) return
    const path = markdownLinkPath(event.target.closest("a[href]")?.getAttribute("href") ?? "")
    if (path === undefined) return
    event.preventDefault()
    void copyText(path).catch(() => {})
  }}>
    <Streamdown isAnimating={streaming} controls={false}>{text}</Streamdown>
  </div>
}

function MessageControls({ text, actions, user }: { text: string; actions?: MessageActions; user?: UserMessage }) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle")
  const hasText = text.trim() !== ""
  return <div className="rw-message-controls" role="group" aria-label="Message actions">
    {hasText && <Button size="sm" variant="tertiary" isIconOnly aria-label={copyState === "copied" ? "Message copied" : "Copy message"}
      onPress={() => { void copyText(text).then(() => setCopyState("copied"), () => setCopyState("failed")) }}>
      {copyState === "copied" ? <Check size={15} /> : <Copy size={15} />}
    </Button>}
    {hasText && actions?.quote && <Button size="sm" variant="tertiary" isIconOnly aria-label="Quote message" onPress={() => actions.quote?.(text)}><Quote size={15} /></Button>}
    {user && actions?.branch && <Button size="sm" variant="tertiary" isIconOnly aria-label="Start a line from this message"
      isDisabled={!user.settled} onPress={() => actions.branch?.(user)}><GitBranch size={15} /></Button>}
    {user && actions?.fork && <Button size="sm" variant="tertiary" isIconOnly aria-label="Fork before this message"
      isDisabled={!user.settled} onPress={() => actions.fork?.(user)}><GitFork size={15} /></Button>}
    {copyState === "failed" && <small className="rw-error" role="alert">Clipboard access failed.</small>}
  </div>
}

function TranscriptRow({ row, sessionId, actions }: { row: TranscriptItem; sessionId: string; actions?: MessageActions }) {
  switch (row.kind) {
    case "user":
      return <article className="rw-user" aria-label="Your message">
        {row.text && <div className="rw-user-bubble">{skillSegments(row.text).map(({ text, skill }, index) =>
          skill ? <span className="rw-skill-mark" key={index}>{text}</span> : text)}</div>}
        {row.images?.map((image) => <Image key={image.source.fileId} sessionId={sessionId} fileId={image.source.fileId} />)}
        <MessageControls text={row.text} actions={actions} user={row} />
      </article>
    case "assistant": return <article className="rw-assistant" aria-label="Assistant">
      <Markdown text={row.text} streaming={row.streaming} />
      {!row.streaming && <MessageControls text={row.text} actions={actions} />}
    </article>
    case "error": return <p className="rw-error" role="alert">{row.text}</p>
    case "stopped": return <p className="rw-notice">Stopped</p>
    case "line": return <p className="rw-notice">{row.failed ? "Line failed" : "Line opened"} · {row.purpose}</p>
    case "line-ended": return <p className="rw-notice">Line ended</p>
    case "source-message": return <article><small>{row.from}</small><Markdown text={row.body} /></article>
    case "source-idle": return <p className="rw-notice">{row.branch} · {row.status}</p>
    case "source-ended": return <p className="rw-notice">{row.branch} ended</p>
    case "hook-follow-up": return <Markdown text={row.body} />
  }
}
