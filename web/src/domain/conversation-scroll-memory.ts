export type ConversationView = string

type ViewPositions = Map<ConversationView, { top: number; following?: boolean }>

export class ConversationScrollMemory {
  private readonly positions = new Map<string, ViewPositions>()

  read(sessionId: string, view: ConversationView): number | undefined {
    return this.positions.get(sessionId)?.get(view)?.top
  }

  following(sessionId: string, view: ConversationView): boolean | undefined {
    return this.positions.get(sessionId)?.get(view)?.following
  }

  write(sessionId: string, view: ConversationView, scrollTop: number, following?: boolean): void {
    const positions = this.positions.get(sessionId) ?? new Map()
    positions.set(view, { top: scrollTop, following })
    this.positions.set(sessionId, positions)
  }
}
