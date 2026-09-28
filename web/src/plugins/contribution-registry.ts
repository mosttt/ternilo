export type ContributionDisposer = () => void

/**
 * Small runtime registry shared by Web extension points.
 *
 * Snapshots are reference-stable for useSyncExternalStore. Registration owns
 * one unique key and returns an idempotent disposer, so unloading a UI plugin
 * removes exactly the contributions it installed.
 */
export class ContributionRegistry<Contribution> {
  readonly #entries = new Map<string, Contribution>()
  readonly #listeners = new Set<() => void>()
  readonly #keyOf: (contribution: Contribution) => string
  readonly #compare?: (left: Contribution, right: Contribution) => number
  #snapshot: readonly Contribution[] = Object.freeze([])

  constructor(
    keyOf: (contribution: Contribution) => string,
    compare?: (left: Contribution, right: Contribution) => number,
  ) {
    this.#keyOf = keyOf
    this.#compare = compare
  }

  getSnapshot = (): readonly Contribution[] => this.#snapshot

  subscribe = (listener: () => void): ContributionDisposer => {
    this.#listeners.add(listener)
    return () => { this.#listeners.delete(listener) }
  }

  register = (contribution: Contribution): ContributionDisposer => {
    const key = this.#keyOf(contribution)
    if (!key) throw new Error('contribution key must not be empty')
    if (this.#entries.has(key)) throw new Error(`contribution "${key}" is already registered`)
    this.#entries.set(key, contribution)
    this.#publish()

    let active = true
    return () => {
      if (!active) return
      active = false
      if (this.#entries.get(key) !== contribution) return
      this.#entries.delete(key)
      this.#publish()
    }
  }

  #publish() {
    const next = [...this.#entries.values()]
    if (this.#compare) next.sort(this.#compare)
    this.#snapshot = Object.freeze(next)
    for (const listener of [...this.#listeners]) listener()
  }
}
