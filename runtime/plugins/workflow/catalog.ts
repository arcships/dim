import { stringify } from 'yaml'
import type { Settings, WorkflowFiles, WorkflowFilesErrorThrown, WorkflowRoot } from '../../../npm/plugin.js'
import { decodeDefinition, decodeFile, issues, payloadBytes, withinDefinitionLimit, WorkflowError, type Definition, type Source, type WorkflowSettings } from './definition.ts'

export interface DefinitionEntry {
  name: string
  source: Source
  package_id?: string
  enabled: boolean
  definition?: Definition
  issues: string[]
}

/** One source read per operation; no definitions or enablement are cached. */
export class Catalog {
  readonly settings: Settings
  readonly files: WorkflowFiles
  readonly packages: { id: string; root: string }[]

  constructor(settings: Settings, files: WorkflowFiles, packages: { id: string; root: string }[]) {
    this.settings = settings
    this.files = files
    this.packages = packages
  }

  async load(cwd: string) {
    const snapshot = await this.settings.read({ source: 'workflow', scope: 'user' })
    if (!snapshot.ok) throw new WorkflowError('definition_source_failed', snapshot.error.message)
    const document = snapshot.value.document as unknown as WorkflowSettings
    const entries: DefinitionEntry[] = Object.entries(document.definitions).map(([name, definition]) => ({
      name, definition, source: 'user', enabled: true,
      issues: withinDefinitionLimit(definition) ? issues(definition, name) : ['Definition exceeds 1 MiB'],
    }))
    const projects = await this.settings.projects({})
    if (!projects.ok) throw new WorkflowError('definition_source_failed', projects.error.message)
    if (!projects.value.projects.some(project => project.cwd === cwd)) {
      throw new WorkflowError('definition_source_failed', 'The workspace is not registered with Settings')
    }
    entries.push(...await this.directory({ project: cwd }, name => document.projectEnabled[cwd]?.[name] === true))
    for (const item of this.packages) entries.push(...await this.directory({ package: item.id }, () => true))
    const pluginNames = new Map<string, number>()
    for (const entry of entries) {
      if (entry.source === 'plugin') pluginNames.set(entry.name, (pluginNames.get(entry.name) ?? 0) + 1)
    }
    for (const entry of entries) {
      if (entry.source === 'plugin' && pluginNames.get(entry.name)! > 1) {
        entry.issues.push(`Definition ${entry.name} occurs in multiple installed packages`)
      }
    }
    return { document, entries, revision: snapshot.value.revision }
  }

  private async directory(root: WorkflowRoot, enabled: (name: string) => boolean): Promise<DefinitionEntry[]> {
    let files
    try { files = await this.files.list({ root }) }
    catch (error) {
      if ('project' in root && (error as WorkflowFilesErrorThrown).code === 'not_found') return []
      throw new WorkflowError('definition_source_failed', (error as Error).message)
    }
    const entries: DefinitionEntry[] = []
    for (const file of files) {
      if (!file.name.endsWith('.yaml')) continue
      const name = file.name.slice(0, -5)
      const entry: DefinitionEntry = {
        name, source: 'package' in root ? 'plugin' : 'project', enabled: enabled(name), issues: [],
        ...('package' in root ? { package_id: root.package } : {}),
      }
      entries.push(entry)
      if (!/^[a-z0-9-]{1,64}$/.test(name) || file.kind !== 'file') {
        entry.issues.push('A definition must be a regular file named <name>.yaml')
        continue
      }
      try {
        entry.definition = decodeFile(await this.files.read({ root, path: file.name }))
        entry.issues = issues(entry.definition, name)
      } catch (error) {
        if (!(error instanceof WorkflowError) && (error as WorkflowFilesErrorThrown).code !== 'invalid_data') {
          throw new WorkflowError('definition_source_failed', (error as Error).message)
        }
        entry.issues.push((error as Error).message)
      }
    }
    return entries
  }

  async save(source: 'user' | 'project', cwd: string | undefined, input: unknown) {
    const definition = decodeDefinition(input)
    const invalid = issues(definition, definition.name)
    if (invalid.length) throw new WorkflowError('invalid_definition', invalid.slice(0, 10).join('; '))
    if (source === 'user') {
      const snapshot = await this.settings.read({ source: 'workflow', scope: 'user' })
      if (!snapshot.ok) throw new WorkflowError('definition_source_failed', snapshot.error.message)
      const document = snapshot.value.document as unknown as WorkflowSettings
      const next = { ...document, definitions: { ...document.definitions, [definition.name]: definition } }
      if (Buffer.byteLength(JSON.stringify(next)) > payloadBytes) {
        throw new WorkflowError('invalid_definition', 'User definitions exceed the 8 MiB frame payload')
      }
      const saved = await this.settings.update({ source: 'workflow', scope: 'user',
        expectedRevision: snapshot.value.revision, document: next,
      })
      if (!saved.ok) throw new WorkflowError(saved.error.code, saved.error.message)
    } else {
      if (!cwd) throw new WorkflowError('invalid_params', 'Project save requires cwd')
      const text = stringify(definition)
      if (Buffer.byteLength(text) > 8 * 1024 * 1024) {
        throw new WorkflowError('invalid_definition', 'Definition file exceeds 8 MiB')
      }
      try {
        await this.files.createDirectory({ root: { project: cwd } })
        await this.files.replace({ root: { project: cwd }, path: `${definition.name}.yaml`,
          bytes: new TextEncoder().encode(text),
        })
      } catch (error) { throw new WorkflowError('definition_source_failed', (error as Error).message) }
    }
    return { name: definition.name }
  }
}

export function select(entries: DefinitionEntry[], name: string, source?: Source): Definition {
  for (const category of source ? [source] : ['project', 'user', 'plugin']) {
    const entry = entries.find(entry => entry.name === name && entry.source === category && entry.enabled)
    if (!entry) continue
    if (entry.issues.length || !entry.definition) throw new WorkflowError('invalid_definition')
    return entry.definition
  }
  throw new WorkflowError('definition_not_found')
}
