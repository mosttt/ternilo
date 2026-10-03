import { asRecord } from '@/lib/utils'

export interface ProviderSource { url: string; title: string }

export class ProviderSourceIndex {
  private documents = new Map<string, Map<string, ProviderSource>>()

  read(response: Record<string, unknown>): ProviderSource[] {
    const state = asRecord(response.provider_state)
    if (state.protocol !== 'anthropic-messages' || !Array.isArray(state.blocks)) return []
    const sources = new Map<string, ProviderSource>()
    const add = (url: unknown, title: unknown) => {
      if (typeof url !== 'string') return
      try {
        const parsed = new URL(url)
        if (!['https:', 'http:'].includes(parsed.protocol) || parsed.username || parsed.password) return
      } catch { return }
      sources.set(url, { url, title: typeof title === 'string' && title ? title : url })
    }
    for (const value of state.blocks) {
      const block = asRecord(value)
      if (block.type !== 'web_fetch_tool_result') continue
      const result = asRecord(block.content), document = asRecord(result.content)
      if (result.type !== 'web_fetch_result') continue
      add(result.url, document.title)
      if (typeof document.title === 'string' && typeof result.url === 'string') {
        const source = sources.get(result.url)
        if (source) {
          const documents = this.documents.get(document.title) ?? new Map<string, ProviderSource>()
          documents.set(source.url, source)
          this.documents.set(document.title, documents)
        }
      }
    }
    for (const value of state.blocks) {
      const block = asRecord(value)
      if (!Array.isArray(block.citations)) continue
      for (const value of block.citations) {
        const citation = asRecord(value)
        if (citation.type === 'web_search_result_location') add(citation.url, citation.title)
        if (typeof citation.document_title === 'string') {
          for (const source of this.documents.get(citation.document_title)?.values() ?? []) add(source.url, source.title)
        }
      }
    }
    return [...sources.values()]
  }
}
