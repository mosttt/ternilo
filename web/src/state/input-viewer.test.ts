import { describe, expect, it } from 'vitest'
import { optimisticInputAuthor } from './input-viewer'

describe('optimistic input identity', () => {
  it('uses the current browser account or direct Local context only', () => {
    expect(optimisticInputAuthor({ local: false, user: { user_id: 'actor', username: 'actor' } })).toEqual({ kind: 'account', user_id: 'actor', username: 'actor' })
    expect(optimisticInputAuthor({ local: true, user: null })).toEqual({ kind: 'local' })
    expect(optimisticInputAuthor({ local: false, user: null })).toBeUndefined()
  })
})
