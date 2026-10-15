import type { Component } from "vue"

let loading: Promise<Component> | undefined

/**
 * markstream-vue, fetched once. Startup calls this to warm the chunk; destructuring only
 * `default` keeps Rollup tree-shaking the package (a bare `import()` or
 * `module => module.default` kept all of it).
 */
export function loadMarkdownRenderer(): Promise<Component> {
  return loading ??= import("markstream-vue").then(({ default: renderer }) => renderer)
}
