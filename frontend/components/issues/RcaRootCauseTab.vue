<script setup lang="ts">
import { AlertTriangle, Lightbulb, Target } from 'lucide-vue-next'
import { marked } from 'marked'
import type { ParsedRcaReport } from '@/composables/useRcaStream'

defineProps<{
  report: ParsedRcaReport
}>()

function renderMd(md: string): string {
  if (!md) return ''
  return marked.parse(md, { async: false }) as string
}
</script>

<template>
  <div class="space-y-4">
    <!-- Hypotheses -->
    <div v-if="report.hypotheses" class="space-y-2">
      <div class="flex items-center gap-2">
        <AlertTriangle class="h-3.5 w-3.5 text-amber-400" />
        <h3 class="text-xs font-semibold text-zinc-200">Hypotheses</h3>
      </div>
      <div class="rounded-lg border border-amber-500/15 bg-amber-500/[0.02] p-3">
        <div class="rca-section text-[11px] text-zinc-300 leading-relaxed" v-html="renderMd(report.hypotheses)" />
      </div>
    </div>

    <!-- Key Findings -->
    <div v-if="report.keyFindings" class="space-y-2">
      <div class="flex items-center gap-2">
        <Lightbulb class="h-3.5 w-3.5 text-blue-400" />
        <h3 class="text-xs font-semibold text-zinc-200">Key Findings</h3>
      </div>
      <div class="rounded-lg border border-blue-500/15 bg-blue-500/[0.02] p-3">
        <div class="rca-section text-[11px] text-zinc-300 leading-relaxed" v-html="renderMd(report.keyFindings)" />
      </div>
    </div>

    <!-- Root Cause (highlighted) -->
    <div v-if="report.rootCause" class="space-y-2">
      <div class="flex items-center gap-2">
        <Target class="h-3.5 w-3.5 text-emerald-400" />
        <h3 class="text-xs font-semibold text-zinc-200">Root Cause</h3>
      </div>
      <div class="rounded-lg border border-emerald-500/30 bg-emerald-500/[0.04] p-4">
        <div class="rca-section rca-root-cause text-[11px] text-zinc-200 leading-relaxed" v-html="renderMd(report.rootCause)" />
      </div>
    </div>

    <!-- Impact -->
    <div v-if="report.impact" class="space-y-2">
      <h3 class="text-[11px] font-semibold text-zinc-400 uppercase tracking-wider">Impact</h3>
      <div class="rounded-lg border border-zinc-700/30 bg-zinc-900/20 p-3">
        <div class="rca-section text-[11px] text-zinc-300 leading-relaxed" v-html="renderMd(report.impact)" />
      </div>
    </div>

    <!-- Raw text fallback when no structured sections parsed -->
    <div v-if="!report.hypotheses && !report.keyFindings && !report.rootCause && report.raw" class="space-y-2">
      <div class="flex items-center gap-2">
        <Target class="h-3.5 w-3.5 text-zinc-400" />
        <h3 class="text-xs font-semibold text-zinc-200">Analysis (partial)</h3>
      </div>
      <div class="rounded-lg border border-zinc-700/30 bg-zinc-900/20 p-3 max-h-[400px] overflow-y-auto">
        <div class="rca-section text-[11px] text-zinc-300 leading-relaxed" v-html="renderMd(report.raw)" />
      </div>
    </div>

    <!-- Empty state -->
    <div v-if="!report.hypotheses && !report.keyFindings && !report.rootCause && !report.raw" class="text-center py-8">
      <p class="text-xs text-zinc-500">Report sections will appear here once the analysis completes.</p>
    </div>
  </div>
</template>

<style scoped>
.rca-section :deep(p) { margin: 0.25rem 0; line-height: 1.6; }
.rca-section :deep(ul), .rca-section :deep(ol) { padding-left: 1.25rem; margin: 0.25rem 0; }
.rca-section :deep(li) { margin: 0.125rem 0; line-height: 1.5; }
.rca-section :deep(strong) { font-weight: 600; color: var(--foreground); }
.rca-section :deep(code) { font-size: 0.625rem; padding: 0.1rem 0.2rem; border-radius: 0.2rem; background: hsl(var(--secondary)); font-family: ui-monospace, monospace; }
.rca-section :deep(blockquote) { border-left: 3px solid hsl(142 71% 45%); background: hsl(142 71% 45% / 0.06); padding: 0.5rem 0.75rem; margin: 0.375rem 0; border-radius: 0.375rem; font-weight: 500; }
.rca-root-cause :deep(blockquote) { font-size: 0.8125rem; }
</style>
