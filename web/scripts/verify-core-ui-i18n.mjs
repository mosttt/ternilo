import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'

const root = path.resolve(import.meta.dirname, '..')
const sourceRoot = path.join(root, 'src')
const resourceRoot = path.join(sourceRoot, 'i18n', 'resources')
const cjk = /\p{Script=Han}/u

// These keys are selected from runtime values rather than appearing as complete
// string literals at their call sites. Keep this list narrow and namespace-aware.
const dynamicKeyPrefixes = new Map([
  ['admin', ['role.']],
  ['model', ['effort.']],
  ['observability', ['jobs.status.', 'lineage.status.']],
  ['settings', ['computers.state.', 'role.']],
  ['trajectory', ['preview.']],
])

// Reserved for keys whose consumer cannot be represented by a literal or one of
// the prefixes above (for example, a future externally supplied contribution).
// Every entry must include a reason; an empty map is intentional today.
const unusedKeyAllowlist = new Map()

function sourceFiles(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const absolute = path.join(directory, entry.name)
    if (entry.isDirectory()) return sourceFiles(absolute)
    if (!/\.(?:ts|tsx)$/.test(entry.name) || /\.test\.(?:ts|tsx)$/.test(entry.name)) return []
    return [absolute]
  })
}

function intentionalLiteral(relative, line) {
  if (relative.startsWith('src/i18n/resources/')) return true
  if (relative === 'src/components/settings/general-settings.tsx') {
    return line.includes('<option value="zh">中文</option>')
  }
  if (relative === 'src/components/workbench/composer-takeover.tsx') {
    return /\/(?:allow\|approve|deny\|reject)\|/.test(line)
  }
  if (relative === 'src/domain/session-time.ts') return line.includes("locale === 'zh'")
  return false
}

function resourceKeys(absolute) {
  const source = fs.readFileSync(absolute, 'utf8')
  const match = source.match(/export const zh\s*=\s*(\{[\s\S]*?\})\s+satisfies\s+Record/)
  if (!match) throw new Error(`could not read zh locale dictionary from ${path.relative(root, absolute)}`)
  const dictionary = Function(`"use strict"; return (${match[1]})`)()
  return Object.keys(dictionary)
}

function hasStaticConsumer(source, key) {
  return source.includes(`'${key}'`)
    || source.includes(`"${key}"`)
    || source.includes(`\`${key}\``)
}

const violations = []
const checked = sourceFiles(sourceRoot)
for (const absolute of checked) {
  const relative = path.relative(root, absolute)
  for (const [index, line] of fs.readFileSync(absolute, 'utf8').split('\n').entries()) {
    if (cjk.test(line) && !intentionalLiteral(relative, line)) {
      violations.push(`${relative}:${index + 1} ${JSON.stringify(line.trim())}`)
    }
  }
}

const consumerSource = checked
  .filter(absolute => !absolute.startsWith(`${resourceRoot}${path.sep}`))
  .map(absolute => fs.readFileSync(absolute, 'utf8'))
  .join('\n')
const unusedKeys = []
for (const entry of fs.readdirSync(resourceRoot, { withFileTypes: true })) {
  if (!entry.isFile() || !entry.name.endsWith('.ts')) continue
  const namespace = path.basename(entry.name, '.ts')
  const prefixes = dynamicKeyPrefixes.get(namespace) ?? []
  const allowed = unusedKeyAllowlist.get(namespace) ?? new Set()
  for (const key of resourceKeys(path.join(resourceRoot, entry.name))) {
    if (prefixes.some(prefix => key.startsWith(prefix))) continue
    if (allowed.has(key)) continue
    if (!hasStaticConsumer(consumerSource, key)) unusedKeys.push(`${namespace}.${key}`)
  }
}

if (unusedKeys.length) {
  console.error('Web UI locale resources contain keys without a production consumer:')
  for (const key of unusedKeys) console.error(`- ${key}`)
  violations.push(...unusedKeys.map(key => `unused locale key ${key}`))
}

if (violations.length) {
  const literalViolations = violations.filter(violation => !violation.startsWith('unused locale key '))
  if (literalViolations.length) {
    console.error('Web UI contains Chinese literals outside locale resources:')
    for (const violation of literalViolations) console.error(`- ${violation}`)
  }
  process.exitCode = 1
} else {
  const dynamicPrefixCount = [...dynamicKeyPrefixes.values()].reduce((total, prefixes) => total + prefixes.length, 0)
  const allowedKeyCount = [...unusedKeyAllowlist.values()].reduce((total, keys) => total + keys.size, 0)
  console.log(`Web UI i18n gate passed (${checked.length} source files, ${dynamicPrefixCount} dynamic prefixes, ${allowedKeyCount} allowlisted keys).`)
}
