/**
 * JSON-RPC 2.0 over the web entry's same-origin `/ws`: `hello` v3, request
 * timeouts, a `daemon.ping` heartbeat, and reconnect with backoff. Replies and
 * notifications are decoded by the readers in `read.ts` before they leave here.
 */
import { PROTOCOL_VERSION, type HelloResult, type Notification } from "./protocol"
import { ProtocolError, readHello, readNotification } from "./read"

export type ConnectionPhase =
  | "connecting"
  | "connected"
  | "reconnecting"
  /** The entry refused the access cookie; only a fresh `dimcode web` link helps. */
  | "unauthenticated"
  | "version-mismatch"

/** A JSON-RPC error reply; `appError` is the server's stable error code. */
export class RpcError extends Error {
  override name = "RpcError"
  constructor(readonly code: number, message: string, readonly appError: string | undefined) {
    super(message)
  }
}

/** The request could not complete because the socket closed or timed out. */
export class ConnectionLost extends Error {
  override name = "ConnectionLost"
}

export type ClientHandlers = {
  phase(phase: ConnectionPhase): void
  /** After every successful `hello`, including those of a reconnect. */
  ready(hello: HelloResult, reconnected: boolean): void
  notification(notification: Notification): void
  /** A notification that does not match the protocol. */
  invalid(error: ProtocolError): void
}

const REQUEST_TIMEOUT_MS = 30_000
const HEARTBEAT_INTERVAL_MS = 20_000
const HEARTBEAT_TIMEOUT_MS = 10_000
const BACKOFF_MS = [500, 1_000, 2_000, 4_000, 8_000, 15_000]

type Pending = {
  resolve(value: unknown): void
  reject(error: Error): void
  timer: ReturnType<typeof setTimeout>
}

export class RpcClient {
  private socket: WebSocket | undefined
  private nextId = 1
  private pending = new Map<number, Pending>()
  private heartbeat: ReturnType<typeof setInterval> | undefined
  private retry: ReturnType<typeof setTimeout> | undefined
  private attempt = 0
  private connectedOnce = false
  private stopped = false

  constructor(private readonly handlers: ClientHandlers) {}

  start(): void {
    this.stopped = false
    this.open()
  }

  stop(): void {
    this.stopped = true
    clearTimeout(this.retry)
    this.socket?.close()
  }

  /** Reconnects now instead of waiting out the backoff. */
  retryNow(): void {
    if (this.socket !== undefined || this.stopped) return
    clearTimeout(this.retry)
    this.open()
  }

  /** Sends one request and decodes its result with `read`. */
  call<T>(method: string, params: Record<string, unknown>, read: (value: unknown) => T, timeoutMs = REQUEST_TIMEOUT_MS): Promise<T> {
    const socket = this.socket
    if (socket === undefined || socket.readyState !== WebSocket.OPEN) return Promise.reject(new ConnectionLost("Not connected"))
    const id = this.nextId++
    return new Promise<unknown>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new ConnectionLost(`${method} timed out`))
      }, timeoutMs)
      this.pending.set(id, { resolve, reject, timer })
      socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }))
    }).then(read)
  }

  private open(): void {
    this.handlers.phase(this.connectedOnce ? "reconnecting" : "connecting")
    const scheme = location.protocol === "https:" ? "wss:" : "ws:"
    const socket = new WebSocket(`${scheme}//${location.host}/ws`)
    this.socket = socket
    let opened = false
    socket.onopen = () => {
      opened = true
      void this.handshake()
    }
    socket.onmessage = (event) => this.receive(event.data)
    socket.onclose = () => {
      if (this.socket !== socket) return
      this.socket = undefined
      clearInterval(this.heartbeat)
      for (const [id, pending] of this.pending) {
        clearTimeout(pending.timer)
        pending.reject(new ConnectionLost("Connection closed"))
        this.pending.delete(id)
      }
      if (this.stopped) return
      if (opened) this.schedule()
      else void this.diagnoseRefusal()
    }
  }

  private async handshake(): Promise<void> {
    let hello: HelloResult
    try {
      hello = await this.call("hello", { protocolVersion: PROTOCOL_VERSION, clientKind: "web", clientName: "dim-web" }, readHello)
    } catch (error) {
      if (error instanceof RpcError && error.appError === "protocol_version") {
        this.stopped = true
        this.handlers.phase("version-mismatch")
      }
      this.socket?.close()
      return
    }
    if (hello.protocolVersion !== PROTOCOL_VERSION) {
      this.stopped = true
      this.handlers.phase("version-mismatch")
      this.socket?.close()
      return
    }
    const reconnected = this.connectedOnce
    this.connectedOnce = true
    this.attempt = 0
    this.heartbeat = setInterval(() => {
      this.call("daemon.ping", {}, () => undefined, HEARTBEAT_TIMEOUT_MS).catch(() => this.socket?.close())
    }, HEARTBEAT_INTERVAL_MS)
    this.handlers.phase("connected")
    this.handlers.ready(hello, reconnected)
  }

  /**
   * A browser does not expose why an upgrade failed, so ask the entry whether
   * the access cookie is still valid. Any other outcome (entry down, closing)
   * is retried.
   */
  private async diagnoseRefusal(): Promise<void> {
    let signedOut = false
    try {
      signedOut = (await fetch("/api/auth/session", { cache: "no-store" })).status === 401
    } catch {
      signedOut = false
    }
    if (this.stopped) return
    if (signedOut) {
      this.stopped = true
      this.handlers.phase("unauthenticated")
    } else {
      this.schedule()
    }
  }

  private schedule(): void {
    this.handlers.phase(this.connectedOnce ? "reconnecting" : "connecting")
    const delay = BACKOFF_MS[Math.min(this.attempt, BACKOFF_MS.length - 1)]!
    this.attempt += 1
    this.retry = setTimeout(() => this.open(), delay)
  }

  private receive(data: unknown): void {
    let message: Record<string, unknown>
    try {
      const parsed: unknown = JSON.parse(String(data))
      if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) throw new ProtocolError("Expected a JSON-RPC object")
      message = parsed as Record<string, unknown>
    } catch (error) {
      this.handlers.invalid(error instanceof ProtocolError ? error : new ProtocolError("Invalid JSON frame"))
      return
    }
    if (typeof message.id === "number") {
      const pending = this.pending.get(message.id)
      if (pending === undefined) return
      this.pending.delete(message.id)
      clearTimeout(pending.timer)
      if ("error" in message) pending.reject(rpcError(message.error))
      else pending.resolve(message.result)
      return
    }
    if (typeof message.method !== "string") return
    let notification: Notification | undefined
    try {
      notification = readNotification(message.method, message.params)
    } catch (error) {
      this.handlers.invalid(error instanceof ProtocolError ? error : new ProtocolError(String(error)))
      return
    }
    if (notification !== undefined) this.handlers.notification(notification)
  }
}

function rpcError(value: unknown): RpcError {
  const error = value !== null && typeof value === "object" ? value as Record<string, unknown> : {}
  const data = error.data !== null && typeof error.data === "object" ? error.data as Record<string, unknown> : {}
  return new RpcError(
    typeof error.code === "number" ? error.code : -32603,
    typeof error.message === "string" ? error.message : "Request failed",
    typeof data.appError === "string" ? data.appError : undefined,
  )
}

/**
 * Trades a `#bootstrap=<token>` fragment for the access cookie. The fragment
 * is removed from the address bar first, so the token never stays in history.
 * A refused token needs no handling of its own: a cookie from an earlier
 * exchange may still be valid, and the socket reports when none is.
 */
export async function exchangeBootstrapToken(): Promise<void> {
  const token = new URLSearchParams(location.hash.slice(1)).get("bootstrap")
  if (token === null) return
  history.replaceState(history.state, "", `${location.pathname}${location.search}`)
  try {
    await fetch("/api/auth/exchange", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token }),
    })
  } catch {
    // The entry is unreachable; connecting reports it.
  }
}
