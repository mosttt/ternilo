import { expect, it } from 'vitest'
import { displayWorkspacePath } from './workspace-markdown'

it('restores display text without interpreting the path as HTML or rewriting attributes', () => {
  const result = displayWorkspacePath('<p><code>&lt;local-workspace&gt;/README.md</code></p><a href="/&lt;local-workspace&gt;">open</a>', '/home/user/<img src=x onerror=alert(1)>')
  const document = new DOMParser().parseFromString(result, 'text/html')
  expect(document.querySelector('code')?.textContent).toBe('/home/user/<img src=x onerror=alert(1)>/README.md')
  expect(document.querySelector('img')).toBeNull()
  expect(document.querySelector('a')?.getAttribute('href')).toBe('/<local-workspace>')
})

it('keeps redacted source unchanged without a presentation context', () => {
  const html = '<code>&lt;local-workspace&gt;</code>'
  expect(displayWorkspacePath(html, null)).toBe(html)
  expect(displayWorkspacePath(html, '当前工作区「共享项目」')).toBe('<code>当前工作区「共享项目」</code>')
})
