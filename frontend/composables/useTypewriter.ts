/**
 * Typewriter effect for streaming text.
 * Renders text character-by-character at a steady pace (~30-60 chars/sec).
 * Feels like a fast human typist — smooth and readable.
 *
 * Client-only: SSR falls back to instant display (no requestAnimationFrame).
 */
export function useTypewriter(charsPerFrame: number = 2) {
  const displayed = ref('')
  let buffer = ''
  let rafId: number | null = null
  const isClient = import.meta.client

  function feed(text: string) {
    buffer = text
    if (!isClient) {
      displayed.value = buffer
      return
    }
    if (!rafId) tick()
  }

  function tick() {
    rafId = requestAnimationFrame(() => {
      if (displayed.value.length >= buffer.length) {
        rafId = null
        return
      }

      // Steady pace: advance N chars per frame (~60fps → N*60 chars/sec)
      const pos = displayed.value.length + charsPerFrame
      displayed.value = buffer.slice(0, Math.min(pos, buffer.length))

      tick()
    })
  }

  /** Instantly show all buffered text */
  function flush() {
    if (rafId) {
      cancelAnimationFrame(rafId)
      rafId = null
    }
    displayed.value = buffer
  }

  function reset() {
    if (rafId) {
      cancelAnimationFrame(rafId)
      rafId = null
    }
    buffer = ''
    displayed.value = ''
  }

  return { displayed: readonly(displayed), feed, flush, reset }
}
