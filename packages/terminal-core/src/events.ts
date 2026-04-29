/**
 * Tiny typed event-emitter used by the transport and session client.
 *
 * Why not `EventTarget`? Two reasons:
 *  - We want listener callbacks typed per-event, not `Event` blobs cast
 *    by the consumer. EventTarget can do this with a wrapper but the
 *    wrapper is essentially this file.
 *  - The transport runs equally well in Node (vitest) and the browser.
 *    `EventTarget` exists in both, but its semantics differ subtly
 *    around `once` and removal — easier to own a 30-line emitter.
 *
 * Listener errors are caught and dropped. A misbehaving listener must
 * not break the dispatch loop for the others, and the caller is not
 * trying to handle errors from a sibling subscriber.
 */

export type Unsubscribe = () => void;

export type Listener<T> = (event: T) => void;

export class TypedEmitter<EventMap> {
  readonly #listeners: {
    [K in keyof EventMap]?: Set<Listener<EventMap[K]>>;
  } = {};

  on<K extends keyof EventMap>(
    event: K,
    listener: Listener<EventMap[K]>,
  ): Unsubscribe {
    let set = this.#listeners[event];
    if (!set) {
      set = new Set();
      this.#listeners[event] = set;
    }
    set.add(listener);
    return () => {
      this.#listeners[event]?.delete(listener);
    };
  }

  emit<K extends keyof EventMap>(event: K, payload: EventMap[K]): void {
    const set = this.#listeners[event];
    if (!set) {
      return;
    }
    for (const listener of [...set]) {
      try {
        listener(payload);
      } catch {
        // Swallow: a listener throwing must not interrupt sibling listeners.
      }
    }
  }

  removeAll(): void {
    for (const key of Object.keys(this.#listeners) as (keyof EventMap)[]) {
      this.#listeners[key]?.clear();
    }
  }
}
