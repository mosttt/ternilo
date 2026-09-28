import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const docsRoot = path.join(repository, 'docs')
const includeDevelopment = process.argv.includes('--include-development')

function markdownFiles(directory) {
  return readdirSync(directory, { withFileTypes: true })
    .flatMap(entry => {
      const target = path.join(directory, entry.name)
      if (!includeDevelopment && target === path.join(docsRoot, 'development')) return []
      if (entry.isDirectory()) return markdownFiles(target)
      return entry.isFile() && entry.name.endsWith('.md') ? [target] : []
    })
}

const files = [
  ...readdirSync(repository).filter(name => /^README(?:\.[\w-]+)?\.md$/.test(name)).map(name => path.join(repository, name)),
  path.join(repository, 'THIRD_PARTY_NOTICES.md'),
  ...markdownFiles(docsRoot),
]
const failures = []

function githubSlug(value) {
  return value
    .replace(/<[^>]*>/g, '')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/[`*_~]/g, '')
    .trim()
    .toLocaleLowerCase('en-US')
    .replace(/[^\p{L}\p{N}\p{M}\-_ ]/gu, '')
    .replace(/ /g, '-')
}

function anchors(markdown) {
  const seen = new Map()
  const result = new Set()
  for (const line of markdown.split(/\r?\n/)) {
    const match = line.match(/^#{1,6}\s+(.+?)\s*#*\s*$/)
    if (!match) continue
    const base = githubSlug(match[1])
    if (!base) continue
    const duplicate = seen.get(base) ?? 0
    seen.set(base, duplicate + 1)
    result.add(duplicate === 0 ? base : `${base}-${duplicate}`)
  }
  return result
}

const anchorCache = new Map()

function targetAnchors(file) {
  let cached = anchorCache.get(file)
  if (!cached) {
    cached = anchors(readFileSync(file, 'utf8'))
    anchorCache.set(file, cached)
  }
  return cached
}

function decode(value, source) {
  try {
    return decodeURIComponent(value)
  } catch {
    failures.push(`${source}: malformed percent encoding in link target ${value}`)
    return value
  }
}

function verifyTarget(source, rawTarget) {
  const target = rawTarget.replace(/^<|>$/g, '')
  if (!target || /^(?:https?:|mailto:|tel:|data:)/i.test(target)) return

  const [rawPath, rawFragment = ''] = target.split('#', 2)
  const linkPath = decode(rawPath, source)
  const fragment = decode(rawFragment, source)
  const resolved = linkPath
    ? path.resolve(path.dirname(source), linkPath)
    : source

  if (!existsSync(resolved)) {
    failures.push(`${path.relative(repository, source)}: missing link target ${target}`)
    return
  }
  if (!fragment || !resolved.endsWith('.md') || !statSync(resolved).isFile()) return

  if (!targetAnchors(resolved).has(fragment.toLocaleLowerCase('en-US'))) {
    failures.push(`${path.relative(repository, source)}: missing heading #${fragment} in ${path.relative(repository, resolved)}`)
  }
}

for (const file of files) {
  const markdown = readFileSync(file, 'utf8')
  const relative = path.relative(repository, file)

  const localPath = markdown.match(/(?:file:\/\/|\/home\/[^\s`'"<>]+|\/Users\/[^\s`'"<>]+|[A-Za-z]:\\Users\\[^\s`'"<>]+|~\/Desktop(?:\/[^\s`'"<>]*)?)/)
  if (localPath && !file.startsWith(path.join(docsRoot, 'development') + path.sep)) failures.push(`${relative}: development-machine path is not portable: ${localPath[0]}`)

  for (const match of markdown.matchAll(/!?\[[^\]]*\]\((<[^>]+>|[^\s)]+)(?:\s+["'][^"']*["'])?\)/g)) {
    verifyTarget(file, match[1])
  }
  for (const match of markdown.matchAll(/^\[[^\]]+\]:\s*(<[^>]+>|\S+)/gm)) {
    verifyTarget(file, match[1])
  }
}

if (failures.length) {
  console.error(`Formal documentation gate failed (${failures.length}):`)
  for (const failure of failures) console.error(`- ${failure}`)
  process.exit(1)
}

console.log(`Formal documentation gate passed (${files.length} Markdown files).`)
