import { describe, expect, it, vi } from 'vitest'
import { readTranscriptView, transcriptViewStorageKey, writeTranscriptView } from './transcript-view'

describe('transcript view preference', () => {
  it('defaults invalid or absent values to compact', () => {
    expect(readTranscriptView(undefined)).toBe('compact')
    expect(readTranscriptView({ getItem: () => 'dense' })).toBe('compact')
  })

  it('persists an exact supported mode and publishes the change', () => {
    const setItem = vi.fn()
    const dispatchEvent = vi.fn()
    writeTranscriptView({ setItem }, 'normal', { dispatchEvent })
    expect(setItem).toHaveBeenCalledWith(transcriptViewStorageKey, 'normal')
    expect(dispatchEvent.mock.calls[0]?.[0]).toBeInstanceOf(CustomEvent)
    expect((dispatchEvent.mock.calls[0]?.[0] as CustomEvent).detail).toBe('normal')
  })
})
