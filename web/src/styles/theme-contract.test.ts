import { readFileSync, readdirSync } from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'

const styles = readFileSync(path.resolve('src/styles/tokens.css'), 'utf8')

function palette(selector: string, source = styles) {
  const start = source.indexOf(`${selector} {`)
  const block = source.slice(start, source.indexOf('}', start))
  return Object.fromEntries([...block.matchAll(/(--[\w-]+): (#[\da-f]{6});/g)].map(match => [match[1], match[2]]))
}

function luminance(hex: string) {
  const rgb = [1, 3, 5].map(offset => {
    const channel = parseInt(hex.slice(offset, offset + 2), 16) / 255
    return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
  })
  return rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722
}

function contrast(first: string, second: string) {
  const values = [luminance(first), luminance(second)].sort((a, b) => b - a)
  return (values[0] + 0.05) / (values[1] + 0.05)
}

describe('shared interface theme', () => {
  it.each([':root', '.dark'])('keeps normal text and action labels readable in %s', selector => {
    const colors = palette(selector)
    for (const [foreground, background] of [
      ['--foreground', '--background'],
      ['--label-secondary', '--surface-layer-1'],
      ['--label-tertiary', '--background'],
      ['--primary-foreground', '--primary'],
      ['--destructive', '--error-surface'],
      ['--destructive-foreground', '--destructive'],
      ['--warning', '--warning-surface'],
      ['--success', '--success-surface'],
    ]) {
      expect(contrast(colors[foreground], colors[background]), `${selector}: ${foreground} on ${background}`).toBeGreaterThanOrEqual(4.5)
    }
  })

  it.each([':root', '.dark'])('keeps every syntax token readable on code blocks in %s', selector => {
    const background = palette(selector)['--surface-code']
    const syntax = palette(selector, readFileSync(path.resolve('src/styles/syntax.css'), 'utf8'))
    expect(Object.keys(syntax)).toHaveLength(9)
    for (const [token, color] of Object.entries(syntax)) {
      expect(contrast(color, background), `${selector}: ${token}`).toBeGreaterThanOrEqual(4.5)
    }
  })

  it('uses shared theme state instead of component-specific dark-theme attributes', () => {
    const root = path.resolve('src')
    const files = readdirSync(root, { recursive: true, withFileTypes: true })
      .filter(entry => entry.isFile() && /\.(css|tsx?)$/.test(entry.name) && !entry.name.includes('.test.'))
    for (const entry of files) {
      const source = readFileSync(path.join(entry.parentPath, entry.name), 'utf8')
      expect(source, entry.name).not.toMatch(/data-[a-z][a-z0-9-]*-dark-theme/)
    }
  })
})
