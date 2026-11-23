import { Button, Popover } from "@heroui/react"
import { Puzzle, X } from "lucide-react"
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from "react"
import { watch } from "@vue/reactivity"
import type { ExtensionDirectory, PanelActivity, PanelDeclaration, PanelEnvironment, PanelModule } from "../../../npm/ui"
import { createPanelContext, panelTitle, panelUnavailable, resourceUrl } from "../features/extensions/context"
import { messages } from "../features/extensions/messages"
import { appLocale } from "./i18n"
import { readSessionMethods } from "../rpc/read"
import { useExtensionsStore } from "../stores/extensions"
import { appearance, chat, connection, useStoreValue } from "./stores"

const extensions = useExtensionsStore()
const readExtensions = () => ({ directory: extensions.directory, error: extensions.error })
const readLocale = () => appLocale.value
type Extension = ExtensionDirectory["extensions"][number]
type Selection = { pluginName: string; panelId: string }
const compactQuery = "(max-width: 719px)"

export function useExtensionDirectory() {
  return useStoreValue(readExtensions)
}

export function ExtensionMenu({ sessionId, directory, directoryError, connected, active, trigger, select }: {
  sessionId: string; directory: ExtensionDirectory | undefined; directoryError: string; connected: boolean; active: boolean
  trigger: RefObject<HTMLButtonElement | null>; select: (value: Selection) => void
}) {
  const locale = useStoreValue(readLocale)
  const labels = messages[locale === "zh-CN" ? "zh-CN" : "en"]
  const [open, setOpen] = useState(false)
  const [revision, setRevision] = useState(0)
  const [read, setRead] = useState<{ methods?: string[]; error?: string }>()
  const panels = (directory?.extensions ?? []).flatMap((extension) => extension.panels.map((panel) => ({ extension, panel })))
    .sort((a, b) => a.extension.pluginName.localeCompare(b.extension.pluginName) || a.panel.id.localeCompare(b.panel.id))
  const needsMethods = panels.some(({ panel }) => panel.methods.length > 0)

  useEffect(() => {
    setRead(undefined)
    if (!active || !connected || !needsMethods) return
    let valid = true
    void connection.client.call("session.methods", { sessionId }, readSessionMethods).then((methods) => {
      if (valid) setRead({ methods })
    }, (error: unknown) => {
      if (valid) setRead({ error: error instanceof Error ? error.message : String(error) })
    })
    return () => { valid = false }
  }, [sessionId, connected, active, directory?.revision, revision, needsMethods])

  const errors = directory?.errors ?? []
  if (!panels.length && !errors.length && !directoryError) return null
  return <div className="rw-extension-menu">
    <Popover isOpen={active && open} onOpenChange={(value) => {
      setOpen(value)
      if (value) {
        setRead(undefined)
        setRevision((previous) => previous + 1)
      }
    }}>
      <Button ref={trigger} className="rw-extension-trigger" size="sm" variant="tertiary" isDisabled={!connected || (!panels.length && !errors.length)}>
        <Puzzle size={15} aria-hidden="true" /> {labels.extensions}
      </Button>
      <Popover.Content placement="bottom end" className="rw-extension-options">
        <Popover.Dialog aria-label={labels.extensions}>
          {panels.map(({ extension, panel }) => {
            const reason = !connected ? labels.disconnected
              : panel.methods.length > 0 && read?.methods === undefined ? read?.error || labels.checking
                : panelUnavailable(panel, connection.supports, read?.methods ?? [])
            return <Button className="rw-extension-option" size="sm" variant="tertiary" key={JSON.stringify([extension.pluginName, panel.id])}
              isDisabled={Boolean(reason)} onPress={() => {
                setOpen(false)
                select({ pluginName: extension.pluginName, panelId: panel.id })
              }}>
              <span className="rw-extension-option-name">
                <span>{panelTitle(panel, locale)}</span>
                <small>{extension.pluginName}</small>
              </span>
              {reason && <small className="rw-extension-option-status">{reason}</small>}
            </Button>
          })}
          {errors.map(({ pluginName, message }) => <p className="rw-muted" key={pluginName}>{pluginName} · {message}</p>)}
        </Popover.Dialog>
      </Popover.Content>
    </Popover>
    {directoryError && <small role="status" className="rw-error">{directoryError}</small>}
  </div>
}

function PanelSurface({ title, children, close }: { title: string; children: ReactNode; close: () => void }) {
  const [compact, setCompact] = useState(() => matchMedia(compactQuery).matches)
  const dialog = useRef<HTMLDialogElement>(null)
  const section = useRef<HTMLElement>(null)
  useEffect(() => {
    const media = matchMedia(compactQuery)
    const change = () => setCompact(media.matches)
    media.addEventListener("change", change)
    return () => media.removeEventListener("change", change)
  }, [])
  useLayoutEffect(() => {
    const element = dialog.current
    if (element) {
      if (!element.open) element.showModal()
      return () => { element.close() }
    }
    section.current?.focus()
  }, [compact])
  const stop = (event: KeyboardEvent) => event.stopPropagation()
  return compact
    ? <dialog ref={dialog} className="rw-extension-panel" aria-label={title} onKeyDown={stop} onCancel={(event) => { event.preventDefault(); close() }}>{children}</dialog>
    : <section ref={section} className="rw-extension-panel" aria-label={title} tabIndex={-1} onKeyDown={stop}>{children}</section>
}

export function ExtensionPanel({ sessionId, branchId, extension, panel, connected, close }: {
  sessionId: string; branchId: string
  extension: Extension | undefined; panel: PanelDeclaration | undefined; connected: boolean
  close: () => void
}) {
  const locale = useStoreValue(readLocale)
  const labels = messages[locale === "zh-CN" ? "zh-CN" : "en"]
  const [container, setContainer] = useState<HTMLDivElement | null>(null)
  const [failure, setFailure] = useState("")
  const [loading, setLoading] = useState(true)
  const [retry, setRetry] = useState(0)
  const [brokenIcon, setBrokenIcon] = useState("")
  const view = useStoreValue(useCallback(() => chat.views[sessionId]?.status, [sessionId]))
  const branch = useStoreValue(useCallback(() => {
    const session = chat.views[sessionId]
    return branchId === session?.defaultBranchId ? session.status : session?.branches[branchId]?.status
  }, [sessionId, branchId]))
  const state = useStoreValue(useCallback(() => chat.views[sessionId]?.lines.find((line) => line.id === branchId)?.state.kind, [sessionId, branchId]))
  const ended = view === "closed" || state?.startsWith("ended") || state === "creation-failed"
  const ready = view === "ready" && branch === "ready"
  const title = panel ? panelTitle(panel, locale) : labels.extensions
  const icon = panel?.icon && extension ? resourceUrl(extension.generation, panel.icon) : undefined

  useEffect(() => {
    chat.ensureLine(sessionId, branchId)
  }, [sessionId, branchId])

  useLayoutEffect(() => {
    setFailure("")
    setLoading(true)
    if (!container || !ready || ended || !connected || !extension || !panel) return
    let valid = true
    const root = container.shadowRoot ?? container.attachShadow({ mode: "open" })
    let binding: ReturnType<typeof createPanelContext> | undefined
    let unmount: (() => void) | undefined
    const environment = (): PanelEnvironment => Object.freeze({ sessionId, branchId, theme: appearance.resolvedTheme, locale: appLocale.value })
    const activity = (): PanelActivity => {
      const current = chat.views[sessionId]
      const target = branchId === current?.defaultBranchId ? current : current?.branches[branchId]
      return Object.freeze({ sessionRunning: current?.sessionRunning === true,
        branch: Object.freeze({ running: target?.running === true, stopping: target?.stopping === true }) })
    }
    function clear() {
      valid = false
      binding?.dispose()
      binding = undefined
      try { unmount?.() } catch (error) { console.error("Extension cleanup failed", error) }
      unmount = undefined
      root.replaceChildren()
    }
    async function mount(panel: PanelDeclaration, generation: string) {
      try {
        const methods = panel.methods.length ? await connection.client.call("session.methods", { sessionId }, readSessionMethods) : []
        if (!valid) return
        const unavailable = panelUnavailable(panel, connection.supports, methods)
        if (unavailable) throw new Error(unavailable)
        const module: PanelModule = await import(/* @vite-ignore */ resourceUrl(generation, panel.entry))
        if (!valid) return
        if (typeof module.mount !== "function") throw new Error("The extension does not export mount")
        binding = createPanelContext(panel, {
          client: connection.client, environment: environment(),
          observeEnvironment: (listener) => watch(environment, listener, { immediate: true }),
          observeActivity: (listener) => watch(activity, listener, { immediate: true }),
          unavailable(error) { if (valid) { setFailure(error.message); clear() } },
        })
        const dispose = module.mount(root, binding.context)
        if (typeof dispose !== "function") throw new Error("mount must return a cleanup function")
        if (valid) { unmount = dispose; setLoading(false) }
        else dispose()
      } catch (error) {
        if (valid) {
          setFailure(error instanceof Error ? error.message : String(error))
          clear()
        }
      }
    }
    void mount(panel, extension.generation)
    return clear
  }, [container, ready, ended, connected, extension?.generation, panel?.id, sessionId, branchId, retry])

  const status = !connected ? labels.disconnected : ended ? labels.ended : !ready ? labels.syncing
    : !panel ? labels.unavailable : failure || (loading ? labels.loading : "")
  return <PanelSurface title={title} close={close}>
    <header className="rw-extension-header">
      {icon && brokenIcon !== icon ? <img src={icon} width={18} height={18} alt="" onError={() => setBrokenIcon(icon)} /> : <Puzzle size={17} />}
      <strong>{title}</strong>
      <Button size="sm" variant="tertiary" isIconOnly aria-label={labels.close} onPress={close}><X size={16} /></Button>
    </header>
    {status && <div className="rw-extension-status" role="status">{status} {(failure || !panel) && connected && <Button size="sm" variant="secondary"
      onPress={async () => { await extensions.refresh(true); setRetry((value) => value + 1) }}>{labels.retry}</Button>}</div>}
    <div ref={setContainer} className="rw-extension-root" aria-label={title} hidden={Boolean(status)} />
  </PanelSurface>
}
