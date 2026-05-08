<script setup lang="ts">
import { ref, computed, watch } from 'vue'
import { Loader2, Play, Square, Check } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import RcaTimeline from './RcaTimeline.vue'
import RcaRootCauseTab from './RcaRootCauseTab.vue'
import RcaMitigationTab from './RcaMitigationTab.vue'
import { parseRcaReport } from '@/composables/useRcaStream'
import type { RcaStep, InvestigationStep, ParsedRcaReport } from '@/composables/useRcaStream'

const props = defineProps<{
  investigationSteps: InvestigationStep[]
  legacySteps: RcaStep[]
  rcaText: string
  thinkingText: string
  isStreaming: boolean
  isComplete: boolean
  elapsedMs: number
  error: string | null
  canStart: boolean
  hasStoredResult: boolean
  storedRcaText: string
  storedSteps: InvestigationStep[]
  parsedReport: ParsedRcaReport | null
}>()

const emit = defineEmits<{
  start: []
  abort: []
}>()

const activeTab = ref<'timeline' | 'rootcause' | 'mitigation'>('timeline')

const displaySteps = computed(() => {
  const raw = props.investigationSteps.length > 0 ? props.investigationSteps : props.storedSteps
  return raw.filter(s => s.status === 'running' || s.dataText || s.analysis || s.summary !== '(skipped)')
})

const hasInvestigation = computed(() => displaySteps.value.length > 0)

const storedReport = computed<ParsedRcaReport | null>(() => {
  if (!props.storedRcaText) return null
  return parseRcaReport(props.storedRcaText)
})

const effectiveReport = computed(() => props.parsedReport || storedReport.value)

const hasReport = computed(() => {
  const r = effectiveReport.value
  if (!r) return false
  return !!(r.rootCause || r.hypotheses || r.keyFindings || r.raw)
})

const hasMitigation = computed(() => {
  const r = effectiveReport.value
  if (!r) return false
  return !!(r.immediateMitigation || r.longTermImprovements)
})

function formatElapsed(ms: number): string {
  const secs = Math.floor(ms / 1000)
  if (secs < 60) return `${secs}s`
  return `${Math.floor(secs / 60)}m${secs % 60}s`
}

watch(() => props.isComplete, (done) => {
  if (done && hasReport.value) {
    activeTab.value = 'rootcause'
  }
})

watch(() => props.hasStoredResult, (has) => {
  if (has && !hasInvestigation.value && hasReport.value) {
    activeTab.value = 'rootcause'
  }
}, { immediate: true })
</script>

<template>
  <div class="space-y-3">
    <!-- Tab bar -->
    <div v-if="hasInvestigation || hasStoredResult" class="flex items-center gap-0.5 border-b border-zinc-800/40 pb-0">
      <button
        class="px-3 py-1.5 text-[11px] font-medium transition-colors border-b-2 -mb-px"
        :class="activeTab === 'timeline'
          ? 'text-zinc-200 border-primary'
          : 'text-zinc-500 border-transparent hover:text-zinc-300'"
        @click="activeTab = 'timeline'"
      >
        <span class="flex items-center gap-1.5">
          <Loader2 v-if="isStreaming" class="h-3 w-3 animate-spin text-orange-400" />
          <Check v-else-if="isComplete || hasStoredResult" class="h-3 w-3 text-emerald-400" />
          Investigation
          <span v-if="displaySteps.length" class="text-[9px] text-zinc-600 tabular-nums">
            {{ displaySteps.length }}
          </span>
        </span>
      </button>

      <button
        class="px-3 py-1.5 text-[11px] font-medium transition-colors border-b-2 -mb-px"
        :class="activeTab === 'rootcause'
          ? 'text-zinc-200 border-emerald-500'
          : hasReport
            ? 'text-zinc-400 border-transparent hover:text-zinc-300'
            : 'text-zinc-600 border-transparent cursor-not-allowed'"
        :disabled="!hasReport"
        @click="hasReport && (activeTab = 'rootcause')"
      >
        Root cause
      </button>

      <button
        class="px-3 py-1.5 text-[11px] font-medium transition-colors border-b-2 -mb-px"
        :class="activeTab === 'mitigation'
          ? 'text-zinc-200 border-orange-500'
          : hasMitigation
            ? 'text-zinc-400 border-transparent hover:text-zinc-300'
            : 'text-zinc-600 border-transparent cursor-not-allowed'"
        :disabled="!hasMitigation"
        @click="hasMitigation && (activeTab = 'mitigation')"
      >
        Mitigation plan
      </button>

      <!-- Elapsed time -->
      <div class="ml-auto flex items-center gap-2">
        <span v-if="elapsedMs > 0" class="text-[10px] text-zinc-600 tabular-nums">
          {{ formatElapsed(elapsedMs) }}
        </span>
        <Button v-if="isStreaming" variant="ghost" size="icon-sm" class="text-destructive" @click="emit('abort')">
          <Square class="h-3 w-3" />
        </Button>
      </div>
    </div>

    <!-- Tab content -->
    <div class="min-h-[100px]">
      <!-- Timeline tab -->
      <div v-if="activeTab === 'timeline'">
        <RcaTimeline
          v-if="hasInvestigation"
          :steps="displaySteps"
          :thinking-text="thinkingText"
          :is-streaming="isStreaming"
          :is-complete="isComplete || hasStoredResult"
          :elapsed-ms="elapsedMs"
        />

        <!-- Legacy steps -->
        <div v-else-if="legacySteps.length > 0" class="space-y-0 rounded border border-zinc-800/40 bg-zinc-900/30 p-2.5">
          <div
            v-for="(step, idx) in legacySteps"
            :key="step.step"
            class="flex items-start gap-2.5"
            :class="idx > 0 ? 'pt-1.5' : ''"
          >
            <div class="flex flex-col items-center shrink-0">
              <div
                v-if="step.status === 'done'"
                class="h-5 w-5 rounded-full bg-emerald-500/15 border border-emerald-500/60 flex items-center justify-center"
              >
                <Check class="h-3 w-3 text-emerald-400" />
              </div>
              <div
                v-else-if="step.status === 'running'"
                class="h-5 w-5 rounded-full bg-orange-500/15 border border-orange-500/60 flex items-center justify-center"
              >
                <Loader2 class="h-3 w-3 text-orange-400 animate-spin" />
              </div>
              <div
                v-if="idx < legacySteps.length - 1"
                class="w-px flex-1 bg-zinc-800/40 mt-1 min-h-[12px]"
              />
            </div>
            <div class="flex-1 min-w-0 pb-1">
              <div class="text-[11px] font-medium text-zinc-300 leading-5">{{ step.label }}</div>
              <div v-if="step.summary" class="text-[10px] text-zinc-500 mt-0.5">{{ step.summary }}</div>
            </div>
            <span v-if="step.duration_ms" class="text-[10px] text-zinc-600 shrink-0 tabular-nums">
              {{ (step.duration_ms / 1000).toFixed(1) }}s
            </span>
          </div>
        </div>
      </div>

      <!-- Root Cause tab -->
      <div v-else-if="activeTab === 'rootcause'">
        <RcaRootCauseTab v-if="effectiveReport" :report="effectiveReport" />
      </div>

      <!-- Mitigation tab -->
      <div v-else-if="activeTab === 'mitigation'">
        <RcaMitigationTab v-if="effectiveReport" :report="effectiveReport" />
      </div>
    </div>

    <!-- Error -->
    <div v-if="error" class="rounded border border-red-500/20 bg-red-500/5 p-2 text-xs text-red-400">
      {{ error }}
    </div>

    <!-- Start / Re-run buttons -->
    <div v-if="canStart && !isStreaming && !rcaText && !hasStoredResult">
      <Button size="sm" class="bg-gradient-to-r from-primary to-orange-500 text-white hover:brightness-110" @click="emit('start')">
        <Play class="h-3 w-3" />
        开始 RCA 分析
      </Button>
    </div>
    <div v-if="(hasStoredResult || isComplete) && !isStreaming" class="pt-1">
      <Button variant="outline" size="sm" @click="emit('start')">
        <Play class="h-3 w-3" />
        重新分析
      </Button>
    </div>
  </div>
</template>
