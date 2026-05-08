<script setup lang="ts">
import { ref } from 'vue'
import {
  ChevronDown, ChevronRight, Check, Loader2,
  FileText, BarChart3, GitBranch, Cpu, Server, Code2,
} from 'lucide-vue-next'
import { marked } from 'marked'
import type { InvestigationStep } from '@/composables/useRcaStream'

defineProps<{
  step: InvestigationStep
  isLast: boolean
}>()

const expanded = ref(false)

function toolIcon(toolName: string) {
  if (toolName.includes('loki')) return FileText
  if (toolName.includes('metrics')) return BarChart3
  if (toolName.includes('traces')) return GitBranch
  if (toolName.includes('container')) return Cpu
  if (toolName.includes('node')) return Server
  if (toolName.includes('source')) return Code2
  return BarChart3
}

function statusColor(status: string) {
  switch (status) {
    case 'complete': return 'border-emerald-500/40 bg-emerald-500/5'
    case 'running': return 'border-orange-500/40 bg-orange-500/5'
    case 'data_received': return 'border-blue-500/40 bg-blue-500/5'
    case 'analyzing': return 'border-blue-500/40 bg-blue-500/5'
    default: return 'border-zinc-700/40 bg-zinc-800/20'
  }
}

function renderMd(md: string): string {
  if (!md) return ''
  return marked.parse(md, { async: false }) as string
}
</script>

<template>
  <div class="relative">
    <!-- Vertical connector line -->
    <div
      v-if="!isLast"
      class="absolute left-[15px] top-[36px] bottom-0 w-px bg-zinc-700/30"
    />

    <!-- Card -->
    <div
      :class="['rounded-lg border transition-all', statusColor(step.status)]"
    >
      <!-- Header (always visible) -->
      <button
        class="flex items-center gap-2.5 w-full px-3 py-2.5 text-left"
        @click="expanded = !expanded"
      >
        <!-- Status icon -->
        <div
class="shrink-0 h-[30px] w-[30px] rounded-full flex items-center justify-center border"
          :class="{
            'bg-emerald-500/15 border-emerald-500/40': step.status === 'complete',
            'bg-orange-500/15 border-orange-500/40': step.status === 'running',
            'bg-blue-500/15 border-blue-500/40': step.status === 'data_received' || step.status === 'analyzing',
          }"
        >
          <Check v-if="step.status === 'complete'" class="h-3.5 w-3.5 text-emerald-400" />
          <Loader2 v-else-if="step.status === 'running'" class="h-3.5 w-3.5 text-orange-400 animate-spin" />
          <component :is="toolIcon(step.toolName)" v-else class="h-3.5 w-3.5 text-blue-400" />
        </div>

        <!-- Label + reasoning -->
        <div class="flex-1 min-w-0">
          <div class="text-xs font-medium text-zinc-200 flex items-center gap-2">
            <component :is="toolIcon(step.toolName)" class="h-3 w-3 text-zinc-500 shrink-0" />
            {{ step.label }}
          </div>
          <div v-if="step.reasoning" class="text-[10px] text-zinc-500 mt-0.5 line-clamp-1">
            {{ step.reasoning }}
          </div>
        </div>

        <!-- Duration -->
        <span
          v-if="step.durationMs"
          class="text-[10px] text-zinc-500 tabular-nums shrink-0"
        >
          {{ (step.durationMs / 1000).toFixed(1) }}s
        </span>

        <!-- Expand toggle -->
        <component
          :is="expanded ? ChevronDown : ChevronRight"
          class="h-3.5 w-3.5 text-zinc-600 shrink-0"
        />
      </button>

      <!-- Expanded content -->
      <div v-if="expanded" class="px-3 pb-3 space-y-2.5 border-t border-zinc-800/30">
        <!-- Reasoning -->
        <div v-if="step.reasoning" class="pt-2">
          <div class="text-[10px] font-medium text-zinc-500 uppercase tracking-wider mb-1">调查理由</div>
          <div class="text-[11px] text-zinc-400 leading-relaxed italic border-l-2 border-orange-500/30 pl-2.5">
            {{ step.reasoning }}
          </div>
        </div>

        <!-- Raw data -->
        <div v-if="step.dataText">
          <div class="text-[10px] font-medium text-zinc-500 uppercase tracking-wider mb-1">原始数据</div>
          <div class="rounded bg-[#0d0f12] border border-zinc-800/40 p-2.5 max-h-[200px] overflow-y-auto">
            <pre class="text-[10px] text-zinc-400 leading-relaxed whitespace-pre-wrap font-mono">{{ step.dataText }}</pre>
          </div>
        </div>

        <!-- AI Analysis -->
        <div v-if="step.analysis">
          <div class="text-[10px] font-medium text-zinc-500 uppercase tracking-wider mb-1">AI 分析</div>
          <div
            class="rca-step-analysis text-[11px] text-zinc-300 leading-relaxed"
            v-html="renderMd(step.analysis)"
          />
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.rca-step-analysis :deep(p) { margin: 0.25rem 0; }
.rca-step-analysis :deep(ul) { padding-left: 1rem; margin: 0.25rem 0; }
.rca-step-analysis :deep(li) { margin: 0.125rem 0; }
.rca-step-analysis :deep(code) { font-size: 0.625rem; padding: 0.1rem 0.2rem; border-radius: 0.2rem; background: hsl(var(--secondary)); }
.rca-step-analysis :deep(strong) { font-weight: 600; color: var(--foreground); }
.rca-step-analysis :deep(blockquote) { border-left: 2px solid hsl(24 100% 50%); background: hsl(24 100% 50% / 0.05); padding: 0.375rem 0.5rem; margin: 0.25rem 0; border-radius: 0.25rem; }
</style>
