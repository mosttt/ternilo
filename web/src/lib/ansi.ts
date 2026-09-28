import Anser from 'anser'
import type { CSSProperties } from 'react'

export interface AnsiSpan {
  text: string
  style?: CSSProperties
}

export type AnsiLine = AnsiSpan[]

const osc = /\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)?/g
const nonCsiEscape = /\u001b(?!\[)[\u0020-\u002f]*[\u0030-\u007e]?/g
const inertControl = /[\u0000-\u0007\u000b-\u001a\u001c-\u001f\u007f]/g

const basicColor: Record<string, string> = {
  '0,0,0': 'var(--foreground)',
  '255,255,255': 'var(--foreground)',
  '85,85,85': 'var(--muted-foreground)',
  '187,0,0': 'var(--destructive)',
  '255,85,85': 'var(--destructive)',
  '0,187,0': 'var(--success)',
  '0,255,0': 'var(--success)',
  '187,187,0': 'var(--warning)',
  '255,255,85': 'var(--warning)',
  '0,0,187': 'var(--primary)',
  '85,85,255': 'var(--primary)',
}

function visibleControls(value: string) {
  const withoutEscapes = value.replace(osc, '').replace(nonCsiEscape, '')
  const lines = withoutEscapes.replace(/\r\n/g, '\n').split('\n').map(line => {
    const painted = line.includes('\r') ? line.split('\r').at(-1) ?? '' : line
    const characters: string[] = []
    for (const character of painted) {
      if (character === '\b') characters.pop()
      else characters.push(character)
    }
    return characters.join('')
  })
  return lines.join('\n').replace(inertControl, '')
}

function chunkStyle(chunk: ReturnType<typeof Anser.ansiToJson>[number]): CSSProperties | undefined {
  const style: CSSProperties = {}
  const foreground = chunk.fg || chunk.fg_truecolor
  const background = chunk.bg || chunk.bg_truecolor
  if (foreground) style.color = basicColor[foreground.replace(/\s+/g, '')] ?? `rgb(${foreground})`
  if (background) style.backgroundColor = `rgb(${background})`
  for (const decoration of chunk.decorations) {
    if (decoration === 'bold') style.fontWeight = 700
    else if (decoration === 'dim') style.opacity = 0.7
    else if (decoration === 'italic') style.fontStyle = 'italic'
    else if (decoration === 'underline') style.textDecoration = 'underline'
    else if (decoration === 'strikethrough') style.textDecoration = 'line-through'
    else if (decoration === 'hidden') style.visibility = 'hidden'
  }
  return Object.keys(style).length ? style : undefined
}

export function parseAnsiLines(value: string): AnsiLine[] {
  let current: AnsiLine = []
  const lines: AnsiLine[] = [current]
  for (const chunk of Anser.ansiToJson(visibleControls(value), { json: true, remove_empty: true })) {
    const style = chunkStyle(chunk)
    chunk.content.split('\n').forEach((part, index) => {
      if (index > 0) {
        current = []
        lines.push(current)
      }
      if (part) current.push({ text: part, style })
    })
  }
  return lines
}
