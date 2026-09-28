import createDOMPurify from 'dompurify'
import { marked } from 'marked'
import markedKatex from 'marked-katex-extension'
import { highlightToHtml } from './src/domain/streaming-highlight.ts'

const DOMPurify = createDOMPurify(window)

function escapeHtml(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')
}

function safeUrl(value, allowMail = false) {
  try {
    const url = new URL(value)
    if (url.protocol === 'http:' || url.protocol === 'https:' || (allowMail && url.protocol === 'mailto:')) {
      return url.href
    }
  } catch {
    // Relative, fragment and malformed destinations stay inert.
  }
  return null
}

const renderer = {
  code({ text, lang }) {
    const authored = String(lang ?? '').trim().split(/\s+/, 1)[0].toLocaleLowerCase()
    const label = authored || 'text'
    const highlighted = highlightToHtml(text, authored)
    const body = highlighted ?? `<pre tabindex="0"><code class="language-plaintext">${escapeHtml(text)}</code></pre>`
    return `<div class="markdown-code"><div class="markdown-code-head"><span>${escapeHtml(label)}</span><button type="button" class="markdown-copy" aria-label="__TERNILO_COPY_CODE__">__TERNILO_COPY__</button></div>${body}</div>`
  },
  html({ text }) {
    return escapeHtml(text)
  },
  link({ href, title, tokens }) {
    const label = this.parser.parseInline(tokens)
    const safe = safeUrl(href, true)
    if (!safe) return label
    const titleAttribute = title ? ` title="${escapeHtml(title)}"` : ''
    return `<a href="${escapeHtml(safe)}" target="_blank" rel="noopener noreferrer"${titleAttribute}>${label}</a>`
  },
  image({ href, title, text }) {
    const safe = safeUrl(href)
    if (!safe) return escapeHtml(text || href)
    const titleAttribute = title ? ` title="${escapeHtml(title)}"` : ''
    return `<img src="${escapeHtml(safe)}" alt="${escapeHtml(text ?? '')}" loading="lazy" referrerpolicy="no-referrer"${titleAttribute}>`
  },
}

marked.use({
  gfm: true,
  breaks: false,
  pedantic: false,
  renderer,
})
marked.use(markedKatex({
  throwOnError: false,
  strict: 'ignore',
  output: 'mathml',
}))

const sanitizeOptions = {
  USE_PROFILES: { html: true, mathMl: true },
  ALLOW_ARIA_ATTR: true,
  ALLOW_DATA_ATTR: false,
  ADD_ATTR: ['target', 'rel', 'loading', 'referrerpolicy', 'tabindex'],
  FORBID_TAGS: ['form', 'option', 'select', 'style', 'textarea'],
}

DOMPurify.addHook('afterSanitizeAttributes', node => {
  if (node.localName === 'input' && node.getAttribute('type') === 'checkbox') {
    node.setAttribute('aria-label', node.hasAttribute('checked') ? '__TERNILO_TASK_COMPLETED__' : '__TERNILO_TASK_PENDING__')
  }
  if (node.localName === 'math') node.setAttribute('tabindex', '0')
})

function localized(html, labels = {}) {
  return html
    .replaceAll('__TERNILO_COPY_CODE__', escapeHtml(labels.copyCode ?? 'Copy code'))
    .replaceAll('__TERNILO_COPY__', escapeHtml(labels.copy ?? 'Copy'))
    .replaceAll('__TERNILO_TASK_COMPLETED__', escapeHtml(labels.taskCompleted ?? 'Completed task'))
    .replaceAll('__TERNILO_TASK_PENDING__', escapeHtml(labels.taskPending ?? 'Pending task'))
}

export function renderMarkdown(source, labels) {
  const html = marked.parse(String(source), { async: false })
  return localized(`<div class="markdown-body">${DOMPurify.sanitize(html, sanitizeOptions)}</div>`, labels)
}

export function renderStreamingMarkdown(source, labels) {
  // A growing fenced block is already structurally unambiguous. Rendering it
  // through the normal, sanitized Markdown pipeline keeps syntax highlighting
  // visible while the model is still writing and preserves the same token tree
  // when the closing fence arrives. Other incomplete Markdown remains literal
  // so emphasis, tables and HTML cannot reflow on every partial delimiter.
  if (/^ {0,3}(?:`{3,}|~{3,})[^\n]*$/m.test(String(source))) {
    return renderMarkdown(source, labels).replace('class="markdown-body"', 'class="markdown-body markdown-streaming"')
  }
  return `<div class="markdown-body markdown-streaming">${escapeHtml(source)}</div>`
}
