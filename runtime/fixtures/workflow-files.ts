import { definePlugin, type Plugin } from '@arcships/rutis'
import type { ManagementMethodsContribution, MethodReply, WorkflowFiles, WorkflowRoot, WorkflowFilesErrorThrown, Settings, JsonObject } from '../../npm/plugin.js'

let generation = 0
export function fixture(namespace: string) {
  return definePlugin({
    name: `WorkflowFilesFixture:${namespace}`,
    inject: ['workflowFiles', 'settings'],
    provides: { [`managementMethods:${namespace}`]: { describe: 'async', call: 'async' } } as Plugin['provides'],
    apply(ctx) {
      const applied = ++generation
      const files = ctx.use<WorkflowFiles>('workflowFiles')
      const settings = ctx.use<Settings>('settings')
      const methods: ManagementMethodsContribution = {
        async describe() {
          return ['read', 'list', 'replace', 'create_directory', 'settings_read', 'settings_update', 'status'].map(name => ({
            name: `${namespace}.${name}`, mutating: ['replace', 'create_directory', 'settings_update'].includes(name), interruption: 'drain', progress: false,
          }))
        },
        async call(call): Promise<MethodReply> {
          // The fixture deliberately sends caller-supplied selectors to the native decoder.
          const request = call.params as unknown as { root: WorkflowRoot; path: string; bytes: number[] }
          try {
            switch (call.method.slice(namespace.length + 1)) {
              case 'read': {
                const bytes = await files.read(request)
                return { kind: 'value', value: { bytes: Array.from(bytes), typed: bytes instanceof Uint8Array } }
              }
              case 'list': return { kind: 'value', value: JSON.stringify(await files.list(request)) }
              case 'replace':
                await files.replace({ ...request, bytes: new Uint8Array(request.bytes) })
                return { kind: 'value', value: true }
              case 'create_directory':
                await files.createDirectory(request)
                return { kind: 'value', value: true }
              case 'settings_read': return { kind: 'value', value: JSON.stringify(await settings.read({ source: 'workflow', scope: 'user' })) }
              case 'settings_update': {
                const read = await settings.read({ source: 'workflow', scope: 'user' })
                if (!read.ok) return { kind: 'value', value: JSON.stringify(read) }
                return { kind: 'value', value: JSON.stringify(await settings.update({
                  source: 'workflow', scope: 'user', expectedRevision: read.value.revision, document: call.params as JsonObject,
                })) }
              }
              case 'status': return { kind: 'value', value: applied }
              default: throw new Error('unknown fixture method')
            }
          } catch (error) {
            const failure = error as WorkflowFilesErrorThrown
            return { kind: 'value', value: { error: JSON.stringify({
              name: failure.name, code: failure.code, message: failure.message, data: failure.data,
            }) } }
          }
        },
      }
      ctx.provide(`managementMethods:${namespace}`, methods)
    },
  })
}
export default fixture('workflow_files_test')
