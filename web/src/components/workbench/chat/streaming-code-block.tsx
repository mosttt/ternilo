import * as React from 'react'
import {
  resolveHighlightLanguage,
  StreamingHighlightSession,
} from '@/domain/streaming-highlight'

export interface OpenStreamingFence {
  prefix: string
  code: string
  lang?: string
}

export function openStreamingFence(source: string): OpenStreamingFence | null {
  const linePattern = /(^|\n)( {0,3})(`{3,}|~{3,})([^\n]*)/g
  let open: { character: string; length: number; start: number; codeStart: number; lang?: string } | null = null
  for (let match = linePattern.exec(source); match; match = linePattern.exec(source)) {
    const marker = match[3]!
    const suffix = match[4]!.replace(/\r$/, '')
    const lineStart = match.index + match[1]!.length
    const lineEnd = source.indexOf('\n', lineStart)
    if (!open) {
      const lang = suffix.trim().split(/\s+/, 1)[0] || undefined
      open = {
        character: marker[0]!,
        length: marker.length,
        start: lineStart,
        codeStart: lineEnd < 0 ? source.length : lineEnd + 1,
        lang,
      }
      continue
    }
    if (marker[0] === open.character && marker.length >= open.length && suffix.trim() === '') open = null
  }
  return open ? { prefix: source.slice(0, open.start), code: source.slice(open.codeStart), lang: open.lang } : null
}

export function StreamingCodeBlock({ code, lang, copyLabel, copyAria }: {
  code: string
  lang?: string
  copyLabel: string
  copyAria: string
}) {
  const trimmed = code.endsWith('\n') ? code.slice(0, -1) : code
  const session = React.useRef<StreamingHighlightSession | null>(null)
  const cache = React.useRef<{
    lines: readonly (readonly { text: string; style: React.CSSProperties }[])[]
    elements: React.ReactNode[]
  } | null>(null)
  const body = React.useMemo(() => {
    session.current ??= new StreamingHighlightSession()
    const lines = session.current.update(trimmed, lang)
    if (!lines) {
      cache.current = null
      return null
    }
    const previous = cache.current
    const elements = lines.map((line, index) => previous?.lines[index] === line
      ? previous.elements[index]
      : <React.Fragment key={index}>
        {index > 0 && '\n'}
        <span className="line" data-streaming-line={index}>
          {line.map((span, spanIndex) => <span key={spanIndex} style={span.style}>{span.text}</span>)}
        </span>
      </React.Fragment>)
    cache.current = { lines, elements }
    return elements
  }, [trimmed, lang])
  const language = resolveHighlightLanguage(lang) ?? 'plaintext'

  return <div className="markdown-code" data-streaming-code-block="">
    <div className="markdown-code-head">
      <span>{lang || 'text'}</span>
      <button type="button" className="markdown-copy" aria-label={copyAria}>{copyLabel}</button>
    </div>
    {body
      ? <pre className="shiki css-variables" style={{ backgroundColor: 'var(--shiki-background)', color: 'var(--shiki-foreground)' }} tabIndex={0}><code className={`language-${language}`}>{body}</code></pre>
      : <pre tabIndex={0}><code className={`language-${language}`}>{trimmed}</code></pre>}
  </div>
}
