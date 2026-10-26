import { definePlugin, type Plugin } from '@arcships/rutis'
import { validateHeaderValue, STATUS_CODES } from 'node:http'
import type { JsonObject, PartRegistry, Settings, SourceDeclaration, ToolCall, ToolDefinition, ToolRegistry, ToolResult } from '../../../npm/plugin.d.ts'
import row from './plugin.json' with { type: 'json' }
import declarations from './settings.json' with { type: 'json' }
import { convert, plainText, redact } from './content.ts'
import { DOWNLOAD_LIMIT, WebError, domain, get, hostname, parseUrl, proxyUrl, type Diagnostic, type NetworkConfig } from './network.ts'

type WebDocument = {
  allowPrivateHosts: string[]; proxy?: string;
  searchService?: { kind: string; endpoint?: string; apiKey?: string }
}
type Page = { url: string; status: number; media: string; title?: string; text: string; total: number; limited: boolean; expires: number; bytes: number }
type Hit = { title: string; url: string; snippet: string }
const CACHE_LIMIT = 50 * 1024 * 1024
const FETCH_SCHEMA: ToolDefinition['inputSchema'] = {
  type: 'object', required: ['url'], additionalProperties: false,
  properties: {
    url: { type: 'string', maxLength: 2048 },
    start_char: { type: 'integer', minimum: 0 },
    max_chars: { type: 'integer', minimum: 1000, maximum: 50000 },
  },
}
const SEARCH_SCHEMA: ToolDefinition['inputSchema'] = {
  type: 'object', required: ['query'], additionalProperties: false,
  properties: {
    query: { type: 'string', minLength: 1, maxLength: 500 },
    max_results: { type: 'integer', minimum: 1, maximum: 10 },
    allowed_domains: { type: 'array', maxItems: 10, items: { type: 'string' } },
    blocked_domains: { type: 'array', maxItems: 10, items: { type: 'string' } },
  },
}

function integer(value: unknown, fallback: number, min: number, max: number): number {
  if (value === undefined) return fallback
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < min || value > max) throw new WebError('invalid_input', 'Invalid numeric parameter.')
  return value
}
function fields(args: JsonObject, names: string[]) {
  if (Object.keys(args).some(name => !names.includes(name))) throw new WebError('invalid_input', 'Unknown parameter.')
}
function filters(value: unknown): string[] | undefined {
  if (value === undefined) return undefined
  if (!Array.isArray(value) || value.length > 10) throw new WebError('invalid_input', 'Domain filters need at most ten hostnames.')
  return value.map(item => {
    const host = typeof item === 'string' ? domain(item) : undefined
    if (!host) throw new WebError('invalid_input', 'Domain filters must be hostnames.')
    return host
  })
}
const within = (host: string, names: string[]) => names.some(name => host === name || host.endsWith(`.${name}`))
const hostGroup = (url: URL) => hostname(url).replace(/^www\./, '')

function brave(value: unknown, key: string): Hit[] {
  const bad = () => new WebError('search_failed', 'The search service returned invalid data.')
  if (!value || typeof value !== 'object' || !('type' in value) || value.type !== 'search') throw bad()
  if (!('web' in value)) return []
  const web = value.web
  if (!web || typeof web !== 'object' || !('results' in web) || !Array.isArray(web.results)) throw bad()
  return web.results.map((item: unknown) => {
    if (!item || typeof item !== 'object' || !('title' in item) || typeof item.title !== 'string'
        || !('url' in item) || typeof item.url !== 'string') throw bad()
    const description = 'description' in item ? item.description : ''
    if (typeof description !== 'string') throw bad()
    let url: URL
    try { url = parseUrl(item.url) } catch { throw bad() }
    if (url.href.includes(key) || url.href.includes(encodeURIComponent(key))) throw bad()
    return { title: [...redact(plainText(item.title), key)].slice(0, 500).join(''), url: url.href, snippet: [...redact(plainText(description), key)].slice(0, 500).join('') }
  })
}

// The Session part: each Session mounts one copy.
export const part = definePlugin({
  name: 'Web',
  inject: ['settings', 'toolRegistry'],
  async apply(ctx) {
    const settings = ctx.use<Settings>('settings')
    const cache = new Map<string, Page>()
    const inflight = new Map<AbortController, Promise<ToolResult>>()
    let closed = false
    let cacheBytes = 0
    ctx.effect(async () => {
      closed = true
      for (const controller of inflight.keys()) controller.abort(new WebError('cancelled', 'Call cancelled.'))
      await Promise.allSettled(inflight.values())
      cache.clear()
      cacheBytes = 0
    })

    async function fetchPage(args: JsonObject, document: WebDocument, signal: AbortSignal, diagnostics: Diagnostic[]): Promise<ToolResult> {
      fields(args, ['url', 'start_char', 'max_chars'])
      if (typeof args.url !== 'string' || [...args.url].length > 2048) throw new WebError('invalid_input', 'url is required and must contain at most 2048 characters.')
      let url = parseUrl(args.url)
      url.hash = ''
      const start = integer(args.start_char, 0, 0, Number.MAX_SAFE_INTEGER)
      const count = integer(args.max_chars, 20000, 1000, 50000)
      const config: NetworkConfig = { allowPrivateHosts: document.allowPrivateHosts, proxy: proxyUrl(document.proxy) }
      const key = document.searchService?.apiKey
      let page = cache.get(url.href)
      if (page && page.expires <= Date.now()) {
        cacheBytes -= page.bytes
        cache.delete(url.href)
        page = undefined
      }
      if (!page) {
        for (let hop = 0; ; hop++) {
          const response = await get(url, config, signal, diagnostics)
          if (response.status >= 300 && response.status < 400) {
            if (!response.location) throw new WebError('http_status', `HTTP ${response.status} ${STATUS_CODES[response.status] ?? ''}`)
            let target: URL
            try {
              target = parseUrl(new URL(response.location, url).href)
              target.hash = ''
              if ([...target.href].length > 2048) throw new Error('redirect too long')
            }
            catch { throw new WebError('invalid_input', 'The redirect URL is invalid.') }
            if (hostGroup(target) !== hostGroup(url)) return { content: `Redirected to ${redact(target.href, key)}; call web_fetch with that URL to follow it.` }
            if (hop === 5) throw new WebError('too_many_redirects', 'More than five redirects.')
            url = target
            continue
          }
          if (response.status < 200 || response.status >= 300) throw new WebError('http_status', `HTTP ${response.status} ${STATUS_CODES[response.status] ?? ''}`)
          signal.throwIfAborted()
          const converted = convert(response.body, response.contentType, url, diagnostics)
          const text = redact(converted.text, key)
          const title = converted.title ? [...redact(converted.title, key)].slice(0, 500).join('') : undefined
          let total = 0
          for (const _character of text) total++
          const bytes = Buffer.byteLength(text) + Buffer.byteLength(title ?? '') + Buffer.byteLength(url.href)
          page = cache.get(url.href)
          if (!page || page.expires <= Date.now()) {
            if (page) { cache.delete(url.href); cacheBytes -= page.bytes }
            page = { url: url.href, status: response.status, media: response.contentType.split(';', 1)[0].trim().toLowerCase(), title, text, total, limited: response.downloadLimited, expires: Date.now() + 15 * 60_000, bytes }
            if (bytes <= CACHE_LIMIT) {
              while (cacheBytes + bytes > CACHE_LIMIT) {
                const oldest = cache.keys().next().value!
                cacheBytes -= cache.get(oldest)!.bytes
                cache.delete(oldest)
              }
              cache.set(url.href, page)
              cacheBytes += bytes
            }
          }
          break
        }
      }
      const end = Math.min(start + count, page.total)
      const slice: string[] = []
      let index = 0
      for (const character of page.text) {
        if (index >= start && index < end) slice.push(character)
        if (++index >= end) break
      }
      const truncated = end < page.total
      const title = page.title ? [...redact(page.title, key)].slice(0, 500).join('') : undefined
      const header = [`URL: ${redact(page.url, key)}`, ...(title ? [`Title: ${title}`] : []), `Content-Type: ${redact(page.media, key)}`, `Characters: ${start}-${Math.max(start, end)} of ${page.total}`]
      if (truncated) header.push(`[Truncated: call web_fetch again with start_char=${end} to continue.]`)
      if (page.limited) header.push(`[Download limit reached: the page exceeds ${DOWNLOAD_LIMIT} bytes and later content is missing.]`)
      return {
        content: header.join('\n') + '\n\n' + (page.total === 0 ? '(no text content)' : redact(slice.join(''), key)),
        structuredContent: { url: redact(args.url, key), final_url: redact(page.url, key), status: page.status, content_type: redact(page.media, key), ...(title ? { title } : {}), start, end: Math.max(start, end), total: page.total, truncated, download_limited: page.limited, ...(diagnostics.length ? { diagnostics } : {}) },
      }
    }

    async function search(args: JsonObject, document: WebDocument, signal: AbortSignal, diagnostics: Diagnostic[]): Promise<ToolResult> {
      fields(args, ['query', 'max_results', 'allowed_domains', 'blocked_domains'])
      const query = typeof args.query === 'string' ? args.query.trim() : ''
      if (!query || [...query].length > 500) throw new WebError('invalid_input', 'query must contain 1 to 500 characters.')
      const count = integer(args.max_results, 5, 1, 10)
      const allowed = filters(args.allowed_domains)
      const blocked = filters(args.blocked_domains)
      if (allowed && blocked) throw new WebError('invalid_input', 'Domain filters are mutually exclusive.')
      const service = document.searchService
      if (service?.kind !== 'brave' || !service.apiKey) throw new WebError('search_unavailable', 'No usable search service is configured.')
      let url: URL
      let proxy: URL | undefined
      try {
        url = parseUrl(service.endpoint ?? 'https://api.search.brave.com/res/v1/web/search')
        proxy = proxyUrl(document.proxy)
        validateHeaderValue('X-Subscription-Token', service.apiKey)
      } catch { throw new WebError('search_unavailable', 'The configured search service is unusable.') }
      const operators = allowed?.map(host => `site:${host}`).join(' OR ') ?? blocked?.map(host => `-site:${host}`).join(' ')
      url.searchParams.set('q', operators ? `${query} ${allowed ? `(${operators})` : operators}` : query)
      url.searchParams.set('count', String(count))
      const response = await get(url, { allowPrivateHosts: document.allowPrivateHosts, proxy }, signal, diagnostics, { 'X-Subscription-Token': service.apiKey })
      if (response.status < 200 || response.status >= 300 || response.downloadLimited) throw new WebError('search_failed', 'The search service failed.')
      let value: unknown
      try { value = JSON.parse(response.body.toString('utf8')) }
      catch { throw new WebError('search_failed', 'The search service returned invalid data.') }
      const results = brave(value, service.apiKey).filter(hit => {
        const host = hostname(new URL(hit.url))
        return (!allowed || within(host, allowed)) && (!blocked || !within(host, blocked))
      }).slice(0, count)
      const safeQuery = redact(query, service.apiKey)
      const content = results.length ? `Search results for "${safeQuery}" (${results.length} results)\n\n` + results.map((hit, i) => `[${i + 1}] ${hit.title}\n${hit.url}\n${hit.snippet}`).join('\n\n') : `No results for "${safeQuery}".`
      return { content, structuredContent: { results, ...(diagnostics.length ? { diagnostics } : {}) } }
    }

    function execute(name: string, call: ToolCall, cancelled: Promise<void>): Promise<ToolResult> {
      const controller = new AbortController()
      const cancel = () => controller.abort(new WebError('cancelled', 'Call cancelled.'))
      void cancelled.then(cancel, cancel)
      if (closed) cancel()
      const timer = setTimeout(() => controller.abort(new WebError('timeout', 'Call timed out.')), name === 'web_search' ? 20_000 : 30_000)
      let secret: string | undefined
      let interrupted: (() => void) | undefined
      const diagnostics: Diagnostic[] = []
      const task = (async (): Promise<ToolResult> => {
        await Promise.resolve()
        controller.signal.throwIfAborted()
        const read = await Promise.race([
          settings.read({ source: 'web', scope: 'user' }),
          new Promise<never>((_, reject) => {
            interrupted = () => reject(controller.signal.reason)
            controller.signal.addEventListener('abort', interrupted, { once: true })
          }),
        ])
        controller.signal.throwIfAborted()
        if (!read.ok) throw new Error('Web settings could not be read.')
        // Settings has already validated and defaulted the owner's declared document.
        const document = read.value.document as WebDocument
        secret = document.searchService?.apiKey
        return name === 'web_fetch' ? fetchPage(call.arguments, document, controller.signal, diagnostics) : search(call.arguments, document, controller.signal, diagnostics)
      })().catch((error: unknown): ToolResult => {
        if (!(error instanceof WebError)) throw error
        return { content: `${name} failed: ${error.code}: ${redact(error.message, secret)}`, isError: true, ...(diagnostics.length ? { structuredContent: { diagnostics } } : {}) }
      }).finally(() => {
        clearTimeout(timer)
        if (interrupted) controller.signal.removeEventListener('abort', interrupted)
        inflight.delete(controller)
      })
      inflight.set(controller, task)
      return task
    }
    const tools = ctx.use<ToolRegistry>('toolRegistry')
    for (const name of ['web_search', 'web_fetch']) {
      await tools.register(
        { name, description: name === 'web_search' ? 'Search the web. Returns numbered titles, URLs and snippets.' : 'Read a public web page as text. Use start_char to continue at the returned final URL.', inputSchema: name === 'web_search' ? SEARCH_SCHEMA : FETCH_SCHEMA, source: 'extension', risk: 'network' },
        (call, cancelled) => execute(name, call, cancelled),
      )
    }
  },
})

// The plugin's one row registers the Session part (rutis usage §2).
export default definePlugin({
  name: 'Web', inject: row.inject, provides: row.provides as Plugin['provides'],
  async apply(ctx) {
    for (const declaration of declarations as SourceDeclaration[]) {
      await ctx.use<Settings>('settings').register(declaration)
    }
    await ctx.use<PartRegistry>('partRegistry').registerDefinition('session', 'Web', part)
  },
})
