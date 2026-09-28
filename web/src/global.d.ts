interface TerniloBoot {
  apiToken?: string
  remote?: boolean
  platform?: boolean
  providerAuthoring?: boolean
  offline?: boolean
  home?: string | null
  openConfig?: boolean
}

interface Window {
  __TERNILO_BOOT__?: TerniloBoot
  __TAURI__?: {
    core?: { invoke?: <T>(command: string, args?: Record<string, unknown>) => Promise<T> }
    event?: { listen?: <T>(event: string, handler: (event: { payload: T }) => void) => Promise<() => void> }
  }
}

declare module '../rich-text.source.js' {
  interface MarkdownLabels { copy?: string; copyCode?: string; taskCompleted?: string; taskPending?: string }
  export function renderMarkdown(source: unknown, labels?: MarkdownLabels): string
  export function renderStreamingMarkdown(source: unknown, labels?: MarkdownLabels): string
}
