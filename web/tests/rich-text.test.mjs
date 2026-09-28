import assert from 'node:assert/strict'
import test from 'node:test'
import { JSDOM } from 'jsdom'

const dom = new JSDOM('<!doctype html><html><body></body></html>', { url: 'https://ternilo.test/' })
globalThis.window = dom.window
globalThis.document = dom.window.document
globalThis.Node = dom.window.Node
globalThis.Element = dom.window.Element

const { renderMarkdown, renderStreamingMarkdown } = await import('../rich-text.source.js')

function fragment(source, labels = { copy: '复制', copyCode: '复制代码', taskCompleted: '已完成任务', taskPending: '未完成任务' }) {
  const template = document.createElement('template')
  template.innerHTML = renderMarkdown(source, labels)
  return template.content
}

test('localizes task-list accessibility labels without retaining a fixed language', () => {
  const output = fragment('- [x] done\n- [ ] next', {
    copy: 'Copy', copyCode: 'Copy code', taskCompleted: 'Completed task', taskPending: 'Pending task',
  })
  assert.deepEqual([...output.querySelectorAll('input[type="checkbox"]')].map(input => input.getAttribute('aria-label')), [
    'Completed task', 'Pending task',
  ])
  const labels = [...output.querySelectorAll('[aria-label]')].map(element => element.getAttribute('aria-label'))
  assert.equal(labels.includes('已完成任务'), false)
  assert.equal(labels.includes('未完成任务'), false)
})

test('renders CJK, GFM tables, task lists and highlighted fenced code', () => {
  const output = fragment('中文 **粗体**\n\n- [x] 完成\n\n|列|值|\n|-|-|\n|一|二|\n\n```rust\nfn main() {}\n```')
  assert.equal(output.querySelector('strong')?.textContent, '粗体')
  assert.equal(output.querySelector('input[type=checkbox]')?.checked, true)
  assert.equal(output.querySelector('input[type=checkbox]')?.getAttribute('aria-label'), '已完成任务')
  assert.equal(output.querySelector('table td')?.textContent, '一')
  assert.match(output.querySelector('code.language-rust span[style]')?.getAttribute('style') ?? '', /--shiki-token-/)
  assert.equal(output.querySelector('.markdown-copy')?.getAttribute('aria-label'), '复制代码')
})

test('renders inline and display TeX as native MathML', () => {
  const output = fragment('行内 $x^2$。\n\n$$\\int_0^1 x\\,dx$$')
  assert.ok(output.querySelector('math'))
  assert.equal(output.querySelector('math')?.getAttribute('tabindex'), '0')
  assert.match(output.textContent ?? '', /x/)
})

test('drops executable HTML and unsafe destinations', () => {
  const output = fragment('<script>alert(1)</script><img src=x onerror=alert(2)>\n\n[bad](javascript:alert(3)) ![data](data:image/svg+xml,x) [safe](https://example.com/a)')
  assert.equal(output.querySelector('script'), null)
  assert.equal(output.querySelector('[onerror]'), null)
  assert.equal(output.querySelector('a[href^="javascript:"]'), null)
  assert.equal(output.querySelector('img[src^="data:"]'), null)
  const link = output.querySelector('a[href^="https://example.com/"]')
  assert.equal(link?.getAttribute('target'), '_blank')
  assert.equal(link?.getAttribute('rel'), 'noopener noreferrer')
})

test('keeps safe Markdown images lazy and prevents referrer leakage', () => {
  const output = fragment('![release diagram](https://cdn.example.test/release.png "Release")')
  const image = output.querySelector('img')
  assert.equal(image?.getAttribute('src'), 'https://cdn.example.test/release.png')
  assert.equal(image?.getAttribute('alt'), 'release diagram')
  assert.equal(image?.getAttribute('title'), 'Release')
  assert.equal(image?.getAttribute('loading'), 'lazy')
  assert.equal(image?.getAttribute('referrerpolicy'), 'no-referrer')
})

test('keeps streaming markdown literal until the response settles', () => {
  const html = renderStreamingMarkdown('**未完成 <img src=x onerror=1>')
  assert.match(html, /\*\*未完成/)
  assert.doesNotMatch(html, /<img/)
})

test('highlights an open streaming code fence and keeps its token tree when settled', () => {
  const open = fragment('')
  const openTemplate = document.createElement('template')
  openTemplate.innerHTML = renderStreamingMarkdown('```ts\nconst first: number = 1\nlet tail')
  const settled = fragment('```ts\nconst first: number = 1\nlet tail\n```')
  const streamingCode = openTemplate.content.querySelector('code.language-typescript')
  const settledCode = settled.querySelector('code.language-typescript')
  assert.ok(streamingCode)
  assert.match(streamingCode.innerHTML, /--shiki-token-keyword/)
  assert.equal(streamingCode.innerHTML, settledCode?.innerHTML)
  assert.equal(open.textContent, '')
})
