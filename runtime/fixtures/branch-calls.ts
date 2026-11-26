// A plugin of any language calls a Session's or a line's data face by id with
// the two data operations (#2127). The request's `service`, `method` and
// `args` are the target's own, so a typo fails to compile.
import type { Sessions } from '../../npm/plugin.js'
import type { RouteError, SessionScope } from '../../npm/branch-services.js'

/** Every `sessions` failure carries the envelope's retryability. */
export function retryable(error: import('../../npm/plugin.js').SessionsError): boolean {
  const retryable: boolean = error.retryable
  return retryable
}

/** A Session part reads the owner's structured failure from the routed error. */
export function ownerFailure(error: RouteError): string | null {
  return 'owner' in error ? `${error.owner.name}: ${error.owner.message}` : null
}

export async function readByCalls(
  scope: SessionScope,
  sessions: Sessions,
  sessionId: string,
  inputId: string,
): Promise<void> {
  // Each reply is the target method's own type.
  const withdrawn = await scope.lineCall({
    lineId: 'main',
    service: 'inputQueue',
    method: 'withdraw',
    args: [{ input_id: inputId }],
  })
  const status: string = withdrawn.status
  const sequence = await scope.lineCall({
    lineId: 'main',
    service: 'timeline',
    method: 'sequence',
    args: [],
  })
  const committed: number | bigint = sequence
  const config = await scope.call({ service: 'sessionConfig', method: 'get', args: [] })
  const cwd: string = config.cwd
  // A method that replies nothing reaches a routed caller as `null`.
  const removed: null = await scope.call({
    service: 'sessionLoader',
    method: 'remove',
    args: [{ instanceId: 0 }],
  })
  void [status, committed, cwd, removed]

  // @ts-expect-error a service the route does not resolve is refused.
  void scope.lineCall({ lineId: 'main', service: 'inputQueueX', method: 'withdraw', args: [] })
  // @ts-expect-error a method the service does not have is refused.
  void scope.lineCall({ lineId: 'main', service: 'inputQueue', method: 'submitX', args: [] })
  // @ts-expect-error arguments follow the method's parameters.
  void scope.lineCall({ lineId: 'main', service: 'inputQueue', method: 'withdraw', args: ['wrong'] })
  // @ts-expect-error a Session call reaches a Session face, not a line's.
  void scope.call({ service: 'inputQueue', method: 'withdraw', args: [] })
  // @ts-expect-error a reply keeps the target method's own type.
  void scope.lineCall({ lineId: 'main', service: 'timeline', method: 'sequence', args: [] }).then((reply: string) => reply)
  // @ts-expect-error an operation bound to the calling row is direct-only.
  void scope.call({ service: 'uiResources', method: 'publishFile', args: [{}] })
  // @ts-expect-error a subscription an operation hands the caller is not routed.
  void scope.lineCall({ lineId: 'main', service: 'dispatcher', method: 'listen', args: ['n', [], (_e: unknown) => {}] })
  // @ts-expect-error an operation that needs a caller-side cancellation is not routed.
  void scope.lineCall({ lineId: 'main', service: 'wait', method: 'wait', args: [] })
  // @ts-expect-error a Session service takes no lineId.
  void sessions.call({ sessionId, lineId: 'main', service: 'sessionConfig', method: 'get', args: [] })
  // @ts-expect-error a line call takes no Session id.
  void sessions.call({ lineId: 'main', service: 'timeline', method: 'sequence', args: [] })
  // @ts-expect-error `lineCall` takes no `sessionId`: it reaches its own Session only.
  void scope.lineCall({ sessionId, lineId: 'main', service: 'timeline', method: 'sequence', args: [] })
  // @ts-expect-error `SessionScope.call` takes neither `lineId` nor `sessionId`.
  void scope.call({ sessionId, lineId: 'main', service: 'sessionConfig', method: 'get', args: [] })

  await sessions.call({
    sessionId,
    lineId: 'main',
    service: 'inputQueue',
    method: 'withdraw',
    args: [{ input_id: inputId }],
  })
  await sessions.call({ sessionId, service: 'sessionConfig', method: 'get', args: [] })
}
