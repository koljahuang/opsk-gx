<script setup lang="ts">
import { ref, watch, nextTick } from 'vue'
import {
  Check, Loader2,
  FileText, BarChart3, GitBranch, Cpu, Server, Code2, Eye,
} from 'lucide-vue-next'
import { marked } from 'marked'
import { stepCategory, type InvestigationStep, type StepCategory } from '@/composables/useRcaStream'

const props = defineProps<{
  steps: InvestigationStep[]
  thinkingText: string
  isStreaming: boolean
  isComplete: boolean
  elapsedMs: number
}>()

function categoryLabel(cat: StepCategory): string {
  const map: Record<StepCategory, string> = {
    planning: 'Planning',
    fetching: 'Fetching data',
    analyzing: 'Analyzing',
    steering: 'Steering',
    observations: 'Observations',
    findings: 'Findings',
    root_cause: 'Root cause',
  }
  return map[cat] || cat
}

function toolIcon(toolName: string) {
  if (toolName.includes('discover')) return Eye
  if (toolName.includes('check_service') || toolName.includes('health')) return BarChart3
  if (toolName.includes('search_logs') || toolName.includes('loki')) return FileText
  if (toolName.includes('search_traces') || toolName.includes('traces')) return GitBranch
  if (toolName.includes('query_metrics') || toolName.includes('metrics')) return BarChart3
  if (toolName.includes('fetch_source') || toolName.includes('source')) return Code2
  if (toolName.includes('container')) return Cpu
  if (toolName.includes('node')) return Server
  return BarChart3
}

function relativeTime(startedAt: number): string {
  const diff = Math.floor((Date.now() - startedAt) / 1000)
  if (diff < 60) return `${diff}s ago`
  return `${Math.floor(diff / 60)}m ago`
}

function renderMd(md: string): string {
  if (!md) return ''
  return marked.parse(md, { async: false }) as string
}

const timelineEnd = ref<HTMLElement>()

watch(() => props.steps.length, () => {
  nextTick(() => {
    timelineEnd.value?.scrollIntoView({ behavior: 'smooth', block: 'nearest' })
  })
})
</script>

<template>
  <div class="space-y-0">
    <div
      v-for="(step, idx) in steps"
      :key="step.stepId"
      class="relative grid gap-3"
      style="grid-template-columns: 90px 1fr"
    >
      <!-- Left column: category + time -->
      <div class="flex flex-col items-end pr-3 pt-3 relative">
        <div
          v-if="idx < steps.length - 1 || isStreaming"
          class="absolute right-0 top-8 bottom-0 w-px bg-zinc-700/30"
        />
        <div class="absolute right-[-4px] top-[18px] z-10">
          <div
            class="h-2 w-2 rounded-full"
            :class="{
              'bg-emerald-400': step.status === 'complete',
              'bg-orange-400 animate-pulse': step.status === 'running',
              'bg-blue-400': step.status === 'data_received' || step.status === 'analyzing',
            }"
          />
        </div>
        <span class="text-[10px] font-medium text-zinc-400 leading-tight text-right">
          {{ categoryLabel(stepCategory(step.toolName, step.status)) }}
        </span>
        <span class="text-[9px] text-zinc-600 mt-0.5">
          {{ relativeTime(step.startedAt) }}
        </span>
      </div>

      <!-- Right column: card (always expanded) -->
      <div
        class="rounded-lg border mb-2 transition-all"
        :class="{
          'border-emerald-500/20 bg-emerald-500/[0.02]': step.status === 'complete',
          'border-orange-500/20 bg-orange-500/[0.02]': step.status === 'running',
          'border-blue-500/20 bg-blue-500/[0.02]': step.status === 'data_received' || step.status === 'analyzing',
          'border-zinc-700/30 bg-zinc-900/20': !['complete','running','data_received','analyzing'].includes(step.status),
        }"
      >
        <!-- Card header -->
        <div class="px-3 py-2.5">
          <div class="flex items-center gap-2">
            <component :is="toolIcon(step.toolName)" class="h-3.5 w-3.5 text-zinc-500 shrink-0" />
            <span class="text-xs font-medium text-zinc-200 flex-1">{{ step.label }}</span>
            <span v-if="step.status === 'running'">
              <Loader2 class="h-3 w-3 animate-spin text-orange-400" />
            </span>
            <Check v-else-if="step.status === 'complete'" class="h-3 w-3 text-emerald-400" />
            <span v-if="step.durationMs" class="text-[10px] text-zinc-600 tabular-nums">
              {{ (step.durationMs / 1000).toFixed(1) }}s
            </span>
          </div>
        </div>

        <!-- Content: always visible -->
        <div v-if="step.dataText || step.analysis || step.status === 'running' || step.status === 'data_received'" class="border-t border-zinc-800/30">
          <div class="px-3 py-2.5 space-y-2">
            <!-- Raw data -->
            <div v-if="step.dataText">
              <div class="text-[9px] font-medium text-zinc-600 uppercase tracking-wider mb-1">Raw Data</div>
              <div class="rounded bg-[#0d0f12] border border-zinc-800/40 p-2 max-h-[160px] overflow-y-auto">
                <pre class="text-[10px] text-zinc-400 leading-relaxed whitespace-pre-wrap font-mono">{{ step.dataText }}</pre>
              </div>
            </div>
            <!-- Analysis (always markdown) -->
            <div v-if="step.analysis">
              <div class="text-[9px] font-medium text-zinc-600 uppercase tracking-wider mb-1">Analysis</div>
              <div class="rca-analysis text-[11px] text-zinc-300 leading-relaxed" v-html="renderMd(step.analysis)" />
              <span v-if="step.status === 'analyzing'" class="typing-cursor" />
            </div>
            <!-- Loading indicators -->
            <div v-if="step.status === 'running' && !step.dataText" class="flex items-center gap-1.5 pt-1">
              <Loader2 class="h-3 w-3 animate-spin text-orange-400" />
              <span class="text-[10px] text-zinc-500 italic">Fetching data...</span>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- Thinking indicator -->
    <div v-if="thinkingText && isStreaming" class="grid gap-3" style="grid-template-columns: 90px 1fr">
      <div class="flex flex-col items-end pr-3 pt-2">
        <span class="text-[10px] font-medium text-zinc-400">Planning</span>
      </div>
      <div class="flex items-center gap-2 px-3 py-2 mb-2">
        <Loader2 class="h-3 w-3 animate-spin text-orange-400 shrink-0" />
        <span class="text-[10px] text-zinc-500 italic truncate">{{ thinkingText }}</span>
      </div>
    </div>

    <div ref="timelineEnd" />
  </div>
</template>

<style scoped>
.rca-analysis :deep(p) { margin: 0.25rem 0; }
.rca-analysis :deep(ul) { padding-left: 1rem; margin: 0.25rem 0; }
.rca-analysis :deep(li) { margin: 0.125rem 0; }
.rca-analysis :deep(code) { font-size: 0.625rem; padding: 0.1rem 0.2rem; border-radius: 0.2rem; background: hsl(var(--secondary)); }
.rca-analysis :deep(strong) { font-weight: 600; color: var(--foreground); }

.typing-cursor {
  display: inline-block;
  width: 2px;
  height: 1em;
  background: hsl(24 100% 50%);
  margin-left: 1px;
  vertical-align: text-bottom;
  animation: blink 0.8s step-end infinite;
}

@keyframes blink {
  50% { opacity: 0; }
}
</style>
