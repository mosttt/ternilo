import { describe, expect, it } from 'vitest'
import {
  configRecord,
  fieldLabel,
  fieldText,
  parseFieldText,
  pluginConfigDefaults,
  pluginConfigFields,
  resolveSchema,
  schemaType,
} from './plugin-config'

describe('plugin JSON Schema settings adapter', () => {
  const schema = {
    type: 'object',
    required: ['endpoint', 'attempts'],
    properties: {
      endpoint: { type: 'string', title: 'API endpoint', minLength: 4 },
      attempts: { $ref: '#/$defs/Attempts' },
      headers: { type: 'object' },
      modes: { type: 'array' },
      enabled: { type: 'boolean', default: true },
    },
    $defs: {
      Attempts: { type: 'integer', minimum: 1, maximum: 8, description: 'Retry count' },
    },
  }

  it('resolves local refs and preserves top-level required fields', () => {
    const fields = pluginConfigFields(schema)
    expect(fields.map(({ key, required }) => [key, required])).toEqual([
      ['endpoint', true],
      ['attempts', true],
      ['headers', false],
      ['modes', false],
      ['enabled', false],
    ])
    expect(fields[1]?.schema).toMatchObject({ type: 'integer', minimum: 1, maximum: 8 })
    expect(resolveSchema(schema, { $ref: '#/$defs/Attempts', title: 'Attempts' })).toMatchObject({ type: 'integer', title: 'Attempts' })
  })

  it('parses and bounds scalar and structured fields', () => {
    expect(parseFieldText({ type: 'integer', minimum: 1, maximum: 8 }, '4')).toBe(4)
    expect(parseFieldText({ type: 'integer', minimum: 0 }, '0')).toBe(0)
    expect(() => parseFieldText({ type: 'integer', minimum: 1 }, '0')).toThrow('minimum')
    expect(() => parseFieldText({ type: 'integer' }, '1.5')).toThrow('number')
    expect(parseFieldText({ type: 'object' }, '{"x":1}')).toEqual({ x: 1 })
    expect(() => parseFieldText({ type: 'object' }, '[]')).toThrow('object')
    expect(parseFieldText({ type: 'array' }, '["a"]')).toEqual(['a'])
    expect(() => parseFieldText({ type: 'string', enum: ['a', 'b'] }, 'c')).toThrow('enum')
  })

  it('normalizes display types, values, labels, and non-record configs', () => {
    expect(schemaType({ type: ['null', 'string'] })).toBe('string')
    expect(schemaType({ enum: [null, 2, 3] })).toBe('number')
    expect(fieldText({ token: 'hidden' }, 'object')).toBe('{\n  "token": "hidden"\n}')
    expect(fieldLabel('retry_base_delay', {})).toBe('Retry base delay')
    expect(fieldLabel('baseUrl', {})).toBe('Base Url')
    expect(fieldLabel('ignored', { title: 'Visible title' })).toBe('Visible title')
    expect(configRecord(null)).toEqual({})
    expect(configRecord(['not', 'an', 'object'])).toEqual({})
  })

  it('builds initial settings only from declared field defaults', () => {
    expect(pluginConfigDefaults({
      type: 'object',
      properties: {
        style: { type: 'string', default: 'brief' },
        attempts: { $ref: '#/$defs/Attempts' },
        optional: { type: 'string' },
        nested: {
          type: 'object',
          properties: {
            mode: { type: 'string', default: 'safe' },
            unset: { type: 'boolean' },
          },
        },
      },
      $defs: {
        Attempts: { type: 'integer', default: 3 },
      },
    })).toEqual({ style: 'brief', attempts: 3, nested: { mode: 'safe' } })
    expect(pluginConfigDefaults({ type: 'object', properties: { optional: { type: 'string' } } })).toEqual({})
  })
})
