import { describe, expect, it } from 'vitest'
import { ProviderSourceIndex } from './provider-sources'

const response = (blocks: unknown[]) => ({ provider_state: { protocol: 'anthropic-messages', model: 'claude', blocks } })

describe('provider web sources', () => {
  it('reads saved DeepSeek Responses citations without exposing search-state payloads', () => {
    const saved = { provider_state: { protocol: 'deepseek-responses', model: 'deepseek-v4-pro', blocks: [
      { type: 'web_search_call', id: 'ws-1', results: 'opaque-search-state' },
      { type: 'message', content: [{ type: 'output_text', text: 'Answer', annotations: [
        { type: 'url_citation', url: 'https://example.com/article', title: 'Article', start_index: 0, end_index: 6 },
        { type: 'url_citation', url: 'https://example.com/article', title: 'Article' },
        { type: 'url_citation', url: 'javascript:alert(1)', title: 'Unsafe' },
        { type: 'url_citation', url: 'https://user:secret@example.com', title: 'Private' },
      ] }] },
    ] } }
    const reloaded = JSON.parse(JSON.stringify(saved))
    expect(new ProviderSourceIndex().read(reloaded)).toEqual([{ url: 'https://example.com/article', title: 'Article' }])
    expect(new ProviderSourceIndex().read({ ...saved, provider_state: { ...saved.provider_state, protocol: 'openai-responses' } })).toEqual([])
  })

  it('keeps original search citations and removes duplicates and unsafe destinations', () => {
    const sources = new ProviderSourceIndex().read(response([{ type: 'text', text: 'Answer', citations: [
      { type: 'web_search_result_location', url: 'https://example.com/search', title: 'Source' },
      { type: 'web_search_result_location', url: 'https://example.com/search', title: 'Source' },
      { type: 'web_search_result_location', url: 'javascript:alert(1)', title: 'Unsafe' },
      { type: 'web_search_result_location', url: 'https://user:secret@example.com', title: 'Credential URL' },
    ] }]))
    expect(sources).toEqual([{ url: 'https://example.com/search', title: 'Source' }])
  })

  it('resolves document citations after a paused fetch without displaying document bodies', () => {
    const index = new ProviderSourceIndex()
    const fetched = { url: 'https://example.com/document', title: 'Document' }
    expect(index.read(response([{ type: 'web_fetch_tool_result', content: { type: 'web_fetch_result', url: fetched.url, content: { title: fetched.title, source: { data: 'private document body' } } } }]))).toEqual([fetched])
    expect(index.read(response([{ type: 'text', text: 'Answer', citations: [{ type: 'char_location', document_index: 0, document_title: 'Document', cited_text: 'passage' }] }]))).toEqual([fetched])
    expect(new ProviderSourceIndex().read(response([{ type: 'text', citations: [{ document_title: 'Document' }] }]))).toEqual([])
  })
})
