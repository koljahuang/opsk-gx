/**
 * Chat composable — manages SSE connection to Claude CLI backend.
 * Handles streaming with thinking/text/tool_use/done types.
 * Supports abort, edit+resend, session management, image attachments.
 */

export interface ChatImage {
  data: string       // base64 encoded
  mediaType: string  // image/png, image/jpeg, image/gif, image/webp
  name?: string
}

export interface ChatMessage {
  id: string
  role: 'user' | 'assistant'
  content: string
  type: 'text' | 'thinking' | 'tool_use' | 'tool_result' | 'error'
  toolName?: string
  images?: readonly ChatImage[]
  timestamp: Date
  sessionId?: string
  durationMs?: number
}

interface StreamChunk {
  type: 'init' | 'thinking' | 'text' | 'tool_use' | 'tool_result' | 'done' | 'error'
  content?: string
  session_id?: string
  tool_name?: string
  message?: string
  duration_ms?: number
}

const CHAT_SESSION_KEY = 'opsk-chat-session-id'

export function useChat() {
  const config = useRuntimeConfig()
  const baseURL = config.public.apiBase || ''

  const messages = ref<ChatMessage[]>([])
  const isStreaming = ref(false)
  const currentSessionId = ref<string | null>(null)
  const error = ref<string | null>(null)
  const selectedProviderId = ref<string | null>(null)
  const selectedMcpServerIds = ref<string[]>([])
  const disabledMcpTools = ref<string[]>([])  // "serverId:toolName" format

  let currentAssistantText = ''
  let currentAssistantId = ''
  let currentThinkingId = ''
  let abortController: AbortController | null = null
  let forceNewSession = false

  // Persist currentSessionId to sessionStorage so chat survives page refresh
  if (typeof window !== 'undefined') {
    watch(currentSessionId, (val) => {
      if (val) {
        sessionStorage.setItem(CHAT_SESSION_KEY, val)
      } else {
        sessionStorage.removeItem(CHAT_SESSION_KEY)
      }
    })
  }

  function addUserMessage(text: string, images?: ChatImage[]): ChatMessage {
    const msg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'user',
      content: text,
      type: 'text',
      images: images?.length ? images : undefined,
      timestamp: new Date(),
    }
    messages.value.push(msg)
    return msg
  }

  function findOrCreateAssistantMessage(type: ChatMessage['type'], toolName?: string): ChatMessage {
    if (type === 'text') {
      const existing = messages.value.find(m => m.id === currentAssistantId && m.type === 'text')
      if (existing) return existing

      const msg: ChatMessage = {
        id: crypto.randomUUID(),
        role: 'assistant',
        content: '',
        type: 'text',
        timestamp: new Date(),
      }
      currentAssistantId = msg.id
      messages.value.push(msg)
      return msg
    }

    const msg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'assistant',
      content: '',
      type,
      toolName,
      timestamp: new Date(),
    }
    messages.value.push(msg)
    return msg
  }

  async function sendMessage(text: string, images?: ChatImage[]) {
    if ((!text.trim() && !images?.length) || isStreaming.value) return

    error.value = null
    addUserMessage(text, images)
    await streamResponse(text, images)
  }

  /** Edit a user message and resend — truncates everything after that message */
  async function editAndResend(messageId: string, newText: string) {
    if (isStreaming.value) return

    const idx = messages.value.findIndex(m => m.id === messageId)
    if (idx === -1) return

    // Truncate: keep messages up to (but not including) the edited one
    messages.value = messages.value.slice(0, idx)
    error.value = null

    addUserMessage(newText)
    await streamResponse(newText)
  }

  /** Abort the current stream */
  function abortStream() {
    if (abortController) {
      abortController.abort()
      abortController = null
    }
    isStreaming.value = false
  }

  async function streamResponse(text: string, images?: ChatImage[]) {
    isStreaming.value = true
    currentAssistantText = ''
    currentAssistantId = ''
    currentThinkingId = ''

    abortController = new AbortController()

    try {
      // Force new session when frontend has no session context
      // (prevents backend from auto-resuming a stale session via find_active_session)
      const shouldForceNew = forceNewSession || (!currentSessionId.value && messages.value.length <= 1)

      const payload: Record<string, unknown> = {
        message: text,
        session_id: currentSessionId.value,
        new_session: shouldForceNew || undefined,
        provider_id: selectedProviderId.value || undefined,
        mcp_server_ids: selectedMcpServerIds.value.length ? selectedMcpServerIds.value : undefined,
        disabled_mcp_tools: disabledMcpTools.value.length ? disabledMcpTools.value : undefined,
      }
      // Reset the flag after first send
      forceNewSession = false
      if (images?.length) {
        payload.images = images.map(img => ({
          data: img.data,
          media_type: img.mediaType,
          name: img.name,
        }))
      }

      const response = await fetch(`${baseURL}/api/chat`, {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
        signal: abortController.signal,
      })

      if (!response.ok) {
        const err = await response.json().catch(() => ({ error: response.statusText }))
        throw new Error(err.error || 'Chat request failed')
      }

      const reader = response.body?.getReader()
      if (!reader) throw new Error('No response stream')

      const decoder = new TextDecoder()
      let buffer = ''

      while (true) {
        const { done, value } = await reader.read()
        if (done) break

        buffer += decoder.decode(value, { stream: true })

        const lines = buffer.split('\n')
        buffer = lines.pop() || ''

        for (const line of lines) {
          const trimmed = line.trim()
          if (!trimmed || trimmed === ':ping') continue

          if (trimmed.startsWith('data: ')) {
            const json = trimmed.slice(6)
            if (!json) continue
            try {
              const chunk: StreamChunk = JSON.parse(json)
              handleChunk(chunk)
            } catch {
              // ignore
            }
          }
        }
      }
    } catch (err: unknown) {
      if (err instanceof DOMException && err.name === 'AbortError') {
        // User aborted — not an error
        return
      }
      const msg = err instanceof Error ? err.message : 'Unknown error'
      error.value = msg
      messages.value.push({
        id: crypto.randomUUID(),
        role: 'assistant',
        content: msg,
        type: 'error',
        timestamp: new Date(),
      })
    } finally {
      isStreaming.value = false
      abortController = null
    }
  }

  function handleChunk(chunk: StreamChunk) {
    switch (chunk.type) {
      case 'init':
        if (chunk.session_id) currentSessionId.value = chunk.session_id
        break

      case 'thinking': {
        // Reuse thinking message within the CURRENT response only (scoped by currentThinkingId)
        const existing = currentThinkingId
          ? messages.value.find(m => m.id === currentThinkingId && m.type === 'thinking')
          : null
        if (existing) {
          existing.content = chunk.content || ''
        } else {
          const msg = findOrCreateAssistantMessage('thinking')
          currentThinkingId = msg.id
          msg.content = chunk.content || ''
        }
        break
      }

      case 'text': {
        const msg = findOrCreateAssistantMessage('text')
        const newContent = chunk.content || ''
        // Handle both delta and snapshot text events from Claude CLI.
        // Delta: only new characters → append.  Snapshot: full text so far → replace.
        // Detect snapshot: if new content starts with everything we've accumulated.
        if (currentAssistantText && newContent.startsWith(currentAssistantText)) {
          currentAssistantText = newContent
        } else {
          currentAssistantText += newContent
        }
        msg.content = currentAssistantText
        break
      }

      case 'tool_use': {
        const msg = findOrCreateAssistantMessage('tool_use', chunk.tool_name)
        msg.content = chunk.content || ''
        msg.toolName = chunk.tool_name
        break
      }

      case 'tool_result': {
        // Merge into last tool_use message — use splice to trigger Vue reactivity
        let toolIdx = -1
        for (let i = messages.value.length - 1; i >= 0; i--) {
          if (messages.value[i].type === 'tool_use') { toolIdx = i; break }
        }
        if (toolIdx >= 0) {
          const result = (chunk.content || '').slice(0, 200)
          if (result) {
            const old = messages.value[toolIdx]
            messages.value.splice(toolIdx, 1, { ...old, content: old.content + '\n--- Result ---\n' + result })
          }
        }
        break
      }

      case 'done':
        if (chunk.session_id) currentSessionId.value = chunk.session_id
        if (!currentAssistantText && chunk.content) {
          const msg = findOrCreateAssistantMessage('text')
          msg.content = chunk.content
        }
        break

      case 'error': {
        const errMsg = chunk.message || 'Unknown error'
        // Session expired — clear stale session and notify user to retry
        if (errMsg.includes('No conversation found') || errMsg === 'SESSION_EXPIRED') {
          currentSessionId.value = null
          error.value = null
          messages.value.push({
            id: crypto.randomUUID(),
            role: 'assistant',
            content: 'Session expired. Please send your message again.',
            type: 'error',
            timestamp: new Date(),
          })
          break
        }
        error.value = errMsg
        messages.value.push({
          id: crypto.randomUUID(),
          role: 'assistant',
          content: errMsg,
          type: 'error',
          timestamp: new Date(),
        })
        break
      }
    }
  }

  function clearMessages() {
    messages.value = []
    currentSessionId.value = null
    currentAssistantText = ''
    currentAssistantId = ''
    currentThinkingId = ''
    error.value = null
  }

  function startNewChat() {
    clearMessages()
    forceNewSession = true
  }

  /** Resume an existing session by ID — loads message history from backend */
  async function resumeSession(sessionId: string) {
    currentAssistantText = ''
    currentAssistantId = ''
    error.value = null
    currentSessionId.value = sessionId
    forceNewSession = false

    try {
      const resp = await fetch(`${baseURL}/api/chat/sessions/${encodeURIComponent(sessionId)}/messages`, {
        credentials: 'include',
      })
      if (resp.ok) {
        const rows: Array<{
          id: string
          role: string
          content: string
          msg_type: string
          tool_name: string | null
          images: ChatImage[] | null
          duration_ms: number | null
          created_at: string
        }> = await resp.json()
        messages.value = rows.map(r => ({
          id: r.id,
          role: r.role as 'user' | 'assistant',
          content: r.content,
          type: r.msg_type as ChatMessage['type'],
          toolName: r.tool_name ?? undefined,
          images: r.images ?? undefined,
          timestamp: new Date(r.created_at),
          durationMs: r.duration_ms ?? undefined,
        }))
      } else {
        messages.value = []
      }
    } catch {
      messages.value = []
    }
  }

  /** Restore the last active session from sessionStorage (call once on mount) */
  async function restoreSession() {
    if (typeof window === 'undefined') return
    const saved = sessionStorage.getItem(CHAT_SESSION_KEY)
    if (saved && !currentSessionId.value) {
      await resumeSession(saved)
    }
  }

  // Auto-cleanup when the composable's scope is destroyed (e.g. component unmount)
  if (getCurrentScope()) {
    onScopeDispose(() => {
      abortStream()
    })
  }

  return {
    messages: readonly(messages),
    isStreaming: readonly(isStreaming),
    currentSessionId: readonly(currentSessionId),
    error: readonly(error),
    selectedProviderId,
    selectedMcpServerIds,
    disabledMcpTools,
    sendMessage,
    editAndResend,
    abortStream,
    clearMessages,
    startNewChat,
    resumeSession,
    restoreSession,
  }
}
