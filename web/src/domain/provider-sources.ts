import { asRecord } from '@/lib/utils'

export interface ProviderSource { url: string; title: string }

export class ProviderSourceIndex {
  private documents = new Map<string, Map<string, ProviderSource>>()

  read(response: Record<string, unknown>): ProviderSource[] {
    const state = asRecord(response.provider_state)
    if (!['anthropic-messages', 'deepseek-responses'].includes(String(state.protocol)) || !Array.isArray(state.blocks)) return []
    const sources = new Map<string, ProviderSource>()
    const add = (url: unknown, title: unknown) => {
      if (typeof url !== 'string') return
      try {
        const parsed = new URL(url)
        if (!['https:', 'http:'].includes(parsed.protocol) || parsed.username || parsed.password) return
      } catch { return }
      sources.set(url, { url, title: typeof title === 'string' && title ? title : url })
    }
    if (state.protocol === 'deepseek-responses') {
      for (const value of state.blocks) {
        const item = asRecord(value)
        if (item.type !== 'message' || !Array.isArray(item.content)) continue
        for (const value of item.content) {
          const part = asRecord(value)
          if (part.type !== 'output_text' || !Array.isArray(part.annotations)) continue
          for (const value of part.annotations) {
            const citation = asRecord(value)
            if (citation.type === 'url_citation') add(citation.url, citation.title)
          }
        }
      }
      return [...sources.values()]
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
