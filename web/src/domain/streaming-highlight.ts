import { createCssVariablesTheme, createHighlighterCoreSync } from 'shiki/core'
import { createJavaScriptRegexEngine, defaultJavaScriptRegexConstructor } from 'shiki/engine/javascript'
import langBash from '@shikijs/langs/shellscript'
import langCss from '@shikijs/langs/css'
import langDiff from '@shikijs/langs/diff'
import langJson from '@shikijs/langs/json'
import langMarkdown from '@shikijs/langs/markdown'
import langPython from '@shikijs/langs/python'
import langRust from '@shikijs/langs/rust'
import langSql from '@shikijs/langs/sql'
import langTypescript from '@shikijs/langs/typescript'
import langXml from '@shikijs/langs/xml'
import langYaml from '@shikijs/langs/yaml'
import type { CSSProperties } from 'react'
import type { GrammarState, ThemedToken } from 'shiki/core'

const aliases = new Map<string, string>([
  ['typescript', 'typescript'], ['ts', 'typescript'], ['tsx', 'typescript'],
  ['javascript', 'typescript'], ['js', 'typescript'], ['jsx', 'typescript'],
  ['shellscript', 'shellscript'], ['bash', 'shellscript'], ['sh', 'shellscript'], ['shell', 'shellscript'], ['zsh', 'shellscript'],
  ['json', 'json'], ['jsonc', 'json'], ['rust', 'rust'], ['rs', 'rust'],
  ['python', 'python'], ['py', 'python'], ['css', 'css'], ['diff', 'diff'],
  ['markdown', 'markdown'], ['md', 'markdown'], ['sql', 'sql'],
  ['xml', 'xml'], ['html', 'xml'], ['yaml', 'yaml'], ['yml', 'yaml'],
])

const theme = createCssVariablesTheme({
  name: 'css-variables',
  variablePrefix: '--shiki-',
  fontStyle: true,
})

const regexEngine = createJavaScriptRegexEngine({
  forgiving: true,
  regexConstructor: pattern => defaultJavaScriptRegexConstructor(pattern, {
    lazyCompileLength: Number.POSITIVE_INFINITY,
  }),
})

let singleton: ReturnType<typeof createHighlighterCoreSync> | undefined
function highlighter() {
  singleton ??= createHighlighterCoreSync({
    themes: [theme],
    langs: [
      langTypescript, langBash, langCss, langDiff, langJson, langMarkdown,
      langPython, langRust, langSql, langXml, langYaml,
    ],
    engine: regexEngine,
  })
  return singleton
}

// UTF-16 code-unit limits keep model-generated pathological fences away from
// the synchronous TextMate tokenizer without degrading grammar state.
export const HIGHLIGHT_MAX_CODE_LENGTH = 100_000
export const HIGHLIGHT_MAX_LINE_LENGTH = 10_000

function exceedsHighlightLimit(code: string) {
  if (code.length > HIGHLIGHT_MAX_CODE_LENGTH) return true
  let lineStart = 0
  for (let index = 0; index < code.length; index += 1) {
    if (code[index] !== '\n') continue
    if (index - lineStart > HIGHLIGHT_MAX_LINE_LENGTH) return true
    lineStart = index + 1
  }
  return code.length - lineStart > HIGHLIGHT_MAX_LINE_LENGTH
}

function escapeHtml(value: string) {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')
}

function plainCodeToHtml(code: string, resolved: string) {
  return `<pre tabindex="0"><code class="language-${resolved}">${escapeHtml(code)}</code></pre>`
}

export function resolveHighlightLanguage(lang: string | undefined) {
  return lang ? aliases.get(lang.toLowerCase()) : undefined
}

/** Render a settled code block with the same singleton, grammar and theme as streaming fences. */
export function highlightToHtml(code: string, lang: string | undefined): string | undefined {
  const resolved = resolveHighlightLanguage(lang)
  if (!resolved) return undefined
  if (exceedsHighlightLimit(code)) return plainCodeToHtml(code, resolved)
  return highlighter().codeToHtml(code, {
    lang: resolved,
    theme: 'css-variables',
    tokenizeTimeLimit: 0,
    transformers: [{
      pre(node) {
        node.properties.tabIndex = 0
      },
      code(node) {
        this.addClassToHast(node, `language-${resolved}`)
      },
    }],
  })
}

export interface HighlightSpan {
  text: string
  style: CSSProperties
}

function spanStyle(token: ThemedToken): CSSProperties {
  const style: CSSProperties = { color: token.color }
  const bits = token.fontStyle ?? 0
  if ((bits & 1) !== 0) style.fontStyle = 'italic'
  if ((bits & 2) !== 0) style.fontWeight = 'bold'
  const decorations: string[] = []
  if ((bits & 4) !== 0) decorations.push('underline')
  if ((bits & 8) !== 0) decorations.push('line-through')
  if (decorations.length > 0) style.textDecoration = decorations.join(' ')
  return style
}

function lineSpans(line: ThemedToken[]): HighlightSpan[] {
  const spans: HighlightSpan[] = []
  let whitespace = ''
  line.forEach((token, index) => {
    if (/^\s+$/.test(token.content) && index + 1 < line.length) {
      whitespace += token.content
      return
    }
    spans.push({ text: whitespace + token.content, style: spanStyle(token) })
    whitespace = ''
  })
  return spans
}

export class StreamingHighlightSession {
  private resolved: string | undefined
  private prefix = ''
  private spans: HighlightSpan[][] = []
  private state: GrammarState | undefined
  private lastCode: string | undefined
  private lastLang: string | undefined
  private lastResult: readonly HighlightSpan[][] | undefined

  private reset(resolved: string | undefined) {
    this.resolved = resolved
    this.prefix = ''
    this.spans = []
    this.state = undefined
  }

  private tokenize(resolved: string, text: string) {
    return highlighter().codeToTokensBase(text, {
      lang: resolved,
      theme: 'css-variables',
      tokenizeTimeLimit: 0,
      ...(this.state ? { grammarState: this.state } : {}),
    })
  }

  update(code: string, lang: string | undefined): readonly HighlightSpan[][] | undefined {
    if (code === this.lastCode && lang === this.lastLang) return this.lastResult
    this.lastCode = code
    this.lastLang = lang
    const resolved = resolveHighlightLanguage(lang)
    if (!resolved || exceedsHighlightLimit(code)) {
      this.reset(undefined)
      this.lastResult = undefined
      return undefined
    }
    if (resolved !== this.resolved || !code.startsWith(this.prefix)) this.reset(resolved)
    const rest = code.slice(this.prefix.length)
    const lastNewline = rest.lastIndexOf('\n')
    if (lastNewline >= 0) {
      const completedEnd = rest[lastNewline - 1] === '\r' ? lastNewline - 1 : lastNewline
      const tokens = this.tokenize(resolved, rest.slice(0, completedEnd))
      tokens.forEach(line => this.spans.push(lineSpans(line)))
      this.state = highlighter().getLastGrammarState(tokens)
      this.prefix = code.slice(0, this.prefix.length + lastNewline + 1)
    }
    this.lastResult = [
      ...this.spans,
      ...this.tokenize(resolved, rest.slice(lastNewline + 1)).map(lineSpans),
    ]
    return this.lastResult
  }
}
