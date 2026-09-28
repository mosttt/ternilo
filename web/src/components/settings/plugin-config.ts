export interface JsonSchema {
  $ref?: string
  type?: string | string[]
  title?: string
  description?: string
  default?: unknown
  enum?: unknown[]
  properties?: Record<string, JsonSchema>
  required?: string[]
  minimum?: number
  maximum?: number
  minLength?: number
  maxLength?: number
  items?: JsonSchema
  anyOf?: JsonSchema[]
  oneOf?: JsonSchema[]
  $defs?: Record<string, JsonSchema>
  definitions?: Record<string, JsonSchema>
}

export interface PluginConfigField {
  key: string
  schema: JsonSchema
  required: boolean
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null
}

export function configRecord(value: unknown): Record<string, unknown> {
  return record(value) ?? {}
}

function schemaDefault(root: JsonSchema, source: JsonSchema): unknown {
  const schema = resolveSchema(root, source)
  if (schema.default !== undefined) return schema.default
  if (schemaType(schema) !== 'object') return undefined

  const defaults = Object.fromEntries(
    Object.entries(schema.properties ?? {}).flatMap(([key, field]) => {
      const value = schemaDefault(root, field)
      return value === undefined ? [] : [[key, value]]
    }),
  )
  return Object.keys(defaults).length ? defaults : undefined
}

export function pluginConfigDefaults(schema: Record<string, unknown>): Record<string, unknown> {
  return configRecord(schemaDefault(schema as JsonSchema, schema as JsonSchema))
}

export function resolveSchema(root: JsonSchema, source: JsonSchema): JsonSchema {
  const reference = source.$ref
  if (!reference?.startsWith('#/')) return source
  let value: unknown = root
  for (const part of reference.slice(2).split('/')) {
    const decoded = part.replaceAll('~1', '/').replaceAll('~0', '~')
    value = record(value)?.[decoded]
  }
  const resolved = record(value) as JsonSchema | null
  return resolved ? { ...resolved, ...source, $ref: undefined } : source
}

export function pluginConfigFields(schema: Record<string, unknown>): PluginConfigField[] {
  const root = schema as JsonSchema
  const resolved = resolveSchema(root, root)
  const required = new Set(resolved.required ?? [])
  return Object.entries(resolved.properties ?? {}).map(([key, field]) => ({
    key,
    schema: resolveSchema(root, field),
    required: required.has(key),
  }))
}

export function schemaType(schema: JsonSchema): string {
  const type = Array.isArray(schema.type) ? schema.type.find((value) => value !== 'null') : schema.type
  if (type) return type
  if (schema.enum?.length) {
    const sample = schema.enum.find((value) => value !== null)
    if (typeof sample === 'number') return 'number'
    if (typeof sample === 'boolean') return 'boolean'
    return 'string'
  }
  if (schema.properties) return 'object'
  return 'string'
}

export function fieldText(value: unknown, type: string): string {
  if (value === undefined) return ''
  if (type === 'object' || type === 'array') return JSON.stringify(value, null, 2)
  return String(value)
}

export function parseFieldText(schema: JsonSchema, text: string): unknown {
  const type = schemaType(schema)
  let value: unknown
  if (type === 'integer' || type === 'number') {
    if (!text.trim()) throw new Error('required')
    const parsed = Number(text)
    if (!Number.isFinite(parsed) || (type === 'integer' && !Number.isInteger(parsed))) throw new Error('number')
    if (schema.minimum !== undefined && parsed < schema.minimum) throw new Error('minimum')
    if (schema.maximum !== undefined && parsed > schema.maximum) throw new Error('maximum')
    value = parsed
  } else if (type === 'object' || type === 'array') {
    try { value = JSON.parse(text) }
    catch { throw new Error('json') }
    if (type === 'object' && record(value) === null) throw new Error('object')
    if (type === 'array' && !Array.isArray(value)) throw new Error('array')
  } else {
    value = text
    if (schema.minLength !== undefined && text.length < schema.minLength) throw new Error('minLength')
    if (schema.maxLength !== undefined && text.length > schema.maxLength) throw new Error('maxLength')
  }
  if (schema.enum && !schema.enum.some((candidate) => Object.is(candidate, value))) throw new Error('enum')
  return value
}

export function fieldLabel(key: string, schema: JsonSchema): string {
  if (schema.title?.trim()) return schema.title
  return key
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .replaceAll('_', ' ')
    .replace(/^./, (letter) => letter.toUpperCase())
}
