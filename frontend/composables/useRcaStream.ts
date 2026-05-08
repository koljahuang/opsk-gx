/**
 * Composable for streaming RCA (Root Cause Analysis) results via SSE.
 * Supports agent-driven investigation with per-step data cards.
 */

interface StreamChunk {
  type: string
  content?: string
  message?: string
  session_id?: string
  tool_name?: string
  duration_ms?: number
  // Legacy step fields
  step?: string
  status?: string
  label?: string
  summary?: string
  // New agent step fields
  step_id?: string
  reasoning?: string
  data_text?: string
}

/** Step category for timeline display */
export type StepCategory = 'planning' | 'fetching' | 'analyzing' | 'steering' | 'observations' | 'findings' | 'root_cause'

/** Parsed structured RCA report */
export interface ParsedRcaReport {
  hypotheses: string
  keyFindings: string
  rootCause: string
  impact: string
  immediateMitigation: string
  longTermImprovements: string
  raw: string
}

/** Legacy step (backward compat) */
export interface RcaStep {
  step: string
  status: string
  label: string
  summary?: string
  duration_ms?: number
  startedAt: number
}

/** Agent-driven investigation step with data + analysis */
export interface InvestigationStep {
  stepId: string
  toolName: string
  reasoning: string
  label: string
  status: 'running' | 'data_received' | 'analyzing' | 'complete'
  dataText?: string
  analysis?: string
  summary?: string
  durationMs?: number
  startedAt: number
}

export function stepCategory(toolName: string, status: string): StepCategory {
  if (status === 'running' && !toolName) return 'planning'
  if (toolName.includes('discover')) return 'planning'
  if (toolName.includes('check_service') || toolName.includes('search_logs') ||
      toolName.includes('search_traces') || toolName.includes('query_metrics') ||
      toolName.includes('fetch_source')) {
    return 'fetching'
  }
  if (status === 'analyzing' || status === 'data_received') return 'analyzing'
  return 'planning'
}

function buildHeadingPattern(heading: string): RegExp {
  // Match headings in various formats the agent may produce:
  //   ### Heading, ## Heading, # Heading
  //   ### **Heading**, ### **1️⃣ Heading**
  //   **Heading**, bare Heading at line start
  // Allow optional emoji/number prefix like "1️⃣ ", "5️⃣ ", "🔍 " between ** and heading text
  const emojiPrefix = '(?:[\\p{Emoji_Presentation}\\p{Extended_Pictographic}0-9️⃣]+\\s*)*'
  return new RegExp(`(?:^|\\n)\\s*(?:#{1,3}\\s*)?(?:\\*\\*\\s*)?${emojiPrefix}${heading}[^\\S\\n]*[：:]*[^\\S\\n]*(?:\\*\\*)?[^\\S\\n]*\\n`, 'iu')
}

function buildNextPattern(heading: string): RegExp {
  const emojiPrefix = '(?:[\\p{Emoji_Presentation}\\p{Extended_Pictographic}0-9️⃣]+\\s*)*'
  return new RegExp(`(?:^|\\n)\\s*(?:#{1,3}\\s*)?(?:\\*\\*\\s*)?${emojiPrefix}${heading}`, 'iu')
}

export function extractSection(text: string, heading: string, nextHeadings: string[]): string {
  const pattern = buildHeadingPattern(heading)
  const match = text.match(pattern)
  if (!match) return ''
  const startIdx = match.index! + match[0].length
  let endIdx = text.length
  for (const next of nextHeadings) {
    if (next.toLowerCase() === heading.toLowerCase()) continue
    const nextPattern = buildNextPattern(next)
    const nextMatch = text.slice(startIdx).match(nextPattern)
    if (nextMatch && nextMatch.index !== undefined) {
      endIdx = Math.min(endIdx, startIdx + nextMatch.index)
    }
  }
  return text.slice(startIdx, endIdx).trim()
}

const SECTION_VARIANTS: Record<string, string[]> = {
  hypotheses: ['Hypotheses', 'Hypothesis', '假设', '分析假设'],
  keyFindings: ['Key Findings', 'Findings', 'Issue Summary', '关键发现', '发现', '问题定位', '问题描述', '问题摘要', '问题概述'],
  rootCause: ['Root Cause', 'Root Cause Analysis', '根因', '根因分析', '根本原因', '问题根因', '代码缺陷', '故障原因', '错误原因'],
  impact: ['Impact', 'Affected', '影响', '影响范围', '影响评估', '错误日志证据', '日志证据'],
  immediateMitigation: ['Immediate Mitigation', 'Immediate Actions', 'Mitigation', 'Remediation', 'Fix', '立即行动', '修复建议', '缓解措施', '快速修复', '修复方案', '解决方案', '修复', '紧急修复'],
  longTermImprovements: ['Long-term Improvements', 'Long Term', 'Improvements', 'Prevention', 'Recommendations', '长期改进', '改进建议', '预防措施', '后续改进', '长期建议', '结论'],
}

function extractWithVariants(text: string, key: string, allHeadings: string[]): string {
  const variants = SECTION_VARIANTS[key] || []
  for (const variant of variants) {
    const result = extractSection(text, variant, allHeadings)
    if (result) return result
  }
  return ''
}

export function parseRcaReport(text: string): ParsedRcaReport {
  const allHeadings = Object.values(SECTION_VARIANTS).flat()
  return {
    hypotheses: extractWithVariants(text, 'hypotheses', allHeadings),
    keyFindings: extractWithVariants(text, 'keyFindings', allHeadings),
    rootCause: extractWithVariants(text, 'rootCause', allHeadings),
    impact: extractWithVariants(text, 'impact', allHeadings),
    immediateMitigation: extractWithVariants(text, 'immediateMitigation', allHeadings),
    longTermImprovements: extractWithVariants(text, 'longTermImprovements', allHeadings),
    raw: text,
  }
}

export function useRcaStream() {
  const config = useRuntimeConfig()
  const apiBase = config.public.apiBase || ''

  const rcaText = ref('')
  const thinkingText = ref('')
  const isStreaming = ref(false)
  const isComplete = ref(false)
  const error = ref<string | null>(null)
  const startedAt = ref<number | null>(null)
  const elapsedMs = ref(0)
  const steps = ref<RcaStep[]>([])
  const investigationSteps = ref<InvestigationStep[]>([])

  const parsedReport = computed<ParsedRcaReport | null>(() => {
    const text = rcaText.value
    if (!text) return null
    return parseRcaReport(text)
  })

  let abortController: AbortController | null = null
  let elapsedTimer: ReturnType<typeof setInterval> | null = null

  function startElapsedTimer() {
    startedAt.value = Date.now()
    elapsedTimer = setInterval(() => {
      if (startedAt.value) {
        elapsedMs.value = Date.now() - startedAt.value
      }
    }, 100)
  }

  function stopElapsedTimer() {
    if (elapsedTimer) {
      clearInterval(elapsedTimer)
      elapsedTimer = null
    }
  }

  async function startRca(issueId: string) {
    rcaText.value = ''
    thinkingText.value = ''
    isStreaming.value = true
    isComplete.value = false
    error.value = null
    steps.value = []
    investigationSteps.value = []
    abortController = new AbortController()

    startElapsedTimer()

    try {
      let response = await fetch(`${apiBase}/api/issues/${issueId}/rca`, {
        method: 'POST',
        credentials: 'include',
        signal: abortController.signal,
      })

      if (response.status === 401) {
        const { useAuthStore } = await import('@/stores/auth')
        const authStore = useAuthStore()
        const refreshed = await authStore.refreshAccessToken()
        if (refreshed) {
          response = await fetch(`${apiBase}/api/issues/${issueId}/rca`, {
            method: 'POST',
            credentials: 'include',
            signal: abortController.signal,
          })
        }
      }

      if (!response.ok) {
        const raw = await response.text().catch(() => '')
        let msg = 'RCA request failed'
        try {
          const parsed = JSON.parse(raw)
          msg = parsed.error || parsed.message || msg
        } catch {
          if (raw) msg = raw
        }
        throw new Error(msg)
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
              handleChunk(JSON.parse(json))
            } catch { /* ignore */ }
          }
        }
      }

      if (!isComplete.value && !error.value) {
        isComplete.value = true
      }
    } catch (err: unknown) {
      if (err instanceof DOMException && err.name === 'AbortError') return
      error.value = err instanceof Error ? err.message : 'Unknown error'
    } finally {
      isStreaming.value = false
      abortController = null
      stopElapsedTimer()
    }
  }

  function findStep(stepId: string): InvestigationStep | undefined {
    return investigationSteps.value.find(s => s.stepId === stepId)
  }

  function handleChunk(chunk: StreamChunk) {
    switch (chunk.type) {
      // ─── New agent-driven step chunks ─────────────────────
      case 'step_start': {
        if (!chunk.step_id || !chunk.tool_name) break
        investigationSteps.value.push({
          stepId: chunk.step_id,
          toolName: chunk.tool_name,
          reasoning: chunk.reasoning || '',
          label: chunk.label || chunk.tool_name,
          status: 'running',
          startedAt: Date.now(),
        })
        thinkingText.value = chunk.label || `Using ${chunk.tool_name}...`
        break
      }
      case 'step_data': {
        const s = chunk.step_id ? findStep(chunk.step_id) : undefined
        if (s) {
          s.dataText = chunk.data_text || ''
          s.status = 'data_received'
        }
        break
      }
      case 'step_analysis': {
        const s = chunk.step_id ? findStep(chunk.step_id) : undefined
        if (s) {
          s.analysis = (s.analysis || '') + (chunk.content || '')
          s.status = 'analyzing'
        }
        break
      }
      case 'step_complete': {
        const s = chunk.step_id ? findStep(chunk.step_id) : undefined
        if (s) {
          if (chunk.summary === '(skipped)') {
            const idx = investigationSteps.value.findIndex(x => x.stepId === chunk.step_id)
            if (idx !== -1) investigationSteps.value.splice(idx, 1)
          } else {
            s.status = 'complete'
            s.summary = chunk.summary || s.summary
            s.durationMs = chunk.duration_ms
          }
        }
        thinkingText.value = ''
        break
      }

      // ─── Legacy step chunks (backward compat) ─────────────
      case 'step': {
        if (!chunk.step || !chunk.status || !chunk.label) break
        const existing = steps.value.find(s => s.step === chunk.step)
        if (existing) {
          existing.status = chunk.status
          existing.label = chunk.label
          if (chunk.summary) existing.summary = chunk.summary
          if (chunk.duration_ms != null) existing.duration_ms = chunk.duration_ms
        } else {
          steps.value.push({
            step: chunk.step,
            status: chunk.status,
            label: chunk.label,
            summary: chunk.summary,
            duration_ms: chunk.duration_ms,
            startedAt: Date.now(),
          })
        }
        break
      }

      // ─── Standard chunks ──────────────────────────────────
      case 'thinking':
        thinkingText.value = chunk.content || ''
        break
      case 'text':
        rcaText.value += chunk.content || ''
        break
      case 'tool_use':
      case 'tool_result':
        break
      case 'done':
        isComplete.value = true
        if (chunk.content) {
          rcaText.value = chunk.content
        }
        break
      case 'error':
        error.value = chunk.message || 'RCA analysis failed'
        break
    }
  }

  function abort() {
    abortController?.abort()
    isStreaming.value = false
    stopElapsedTimer()
  }

  function reset() {
    rcaText.value = ''
    thinkingText.value = ''
    isStreaming.value = false
    isComplete.value = false
    error.value = null
    startedAt.value = null
    elapsedMs.value = 0
    steps.value = []
    investigationSteps.value = []
    stopElapsedTimer()
  }

  if (getCurrentScope()) {
    onScopeDispose(() => { abort() })
  }

  return {
    rcaText,
    thinkingText,
    isStreaming,
    isComplete,
    error,
    elapsedMs,
    steps,
    investigationSteps,
    parsedReport,
    startRca,
    abort,
    reset,
  }
}
