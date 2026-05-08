<script setup lang="ts">
import { Wrench, Rocket } from 'lucide-vue-next'
import { marked } from 'marked'
import type { ParsedRcaReport } from '@/composables/useRcaStream'

defineProps<{
  report: ParsedRcaReport
}>()

async function handleCopy(event: MouseEvent) {
  const target = event.target as HTMLElement
  const btn = target.closest('.copy-code-btn')
  if (!btn) return
  const pre = btn.closest('.code-wrapper')?.querySelector('pre')
  if (!pre) return
  try {
    await navigator.clipboard.writeText(pre.textContent || '')
    btn.classList.add('copied')
    setTimeout(() => btn.classList.remove('copied'), 2000)
  } catch { /* ignore */ }
}

function renderWithCopyButtons(md: string): string {
  if (!md) return ''
  const html = marked.parse(md, { async: false }) as string
  return html.replace(
    /<pre><code(?:\s+class="language-(\w+)")?>([\s\S]*?)<\/code><\/pre>/g,
    (_match, lang, code) => {
      const langLabel = lang || ''
      return `<div class="code-wrapper relative group my-2">
        <div class="flex items-center justify-between px-2.5 py-1 bg-zinc-800/60 rounded-t border border-b-0 border-zinc-700/40">
          <span class="text-[9px] text-zinc-500 font-mono">${langLabel}</span>
          <button class="copy-code-btn flex items-center gap-1 text-[9px] text-zinc-500 hover:text-zinc-300 transition-colors" type="button">
            <svg class="h-3 w-3 copy-icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
            <svg class="h-3 w-3 check-icon hidden" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>
            <span class="copy-label">Copy</span>
          </button>
        </div>
        <pre class="!mt-0 !rounded-t-none"><code>${code}</code></pre>
      </div>`
    }
  )
}
</script>

<template>
  <div class="space-y-4" @click="handleCopy">
    <!-- Immediate Mitigation -->
    <div v-if="report.immediateMitigation" class="space-y-2">
      <div class="flex items-center gap-2">
        <Wrench class="h-3.5 w-3.5 text-orange-400" />
        <h3 class="text-xs font-semibold text-zinc-200">Immediate Mitigation</h3>
      </div>
      <div class="rounded-lg border border-orange-500/15 bg-orange-500/[0.02] p-3">
        <div class="rca-mitigation text-[11px] text-zinc-300 leading-relaxed" v-html="renderWithCopyButtons(report.immediateMitigation)" />
      </div>
    </div>

    <!-- Long-term Improvements -->
    <div v-if="report.longTermImprovements" class="space-y-2">
      <div class="flex items-center gap-2">
        <Rocket class="h-3.5 w-3.5 text-purple-400" />
        <h3 class="text-xs font-semibold text-zinc-200">Long-term Improvements</h3>
      </div>
      <div class="rounded-lg border border-purple-500/15 bg-purple-500/[0.02] p-3">
        <div class="rca-mitigation text-[11px] text-zinc-300 leading-relaxed" v-html="renderWithCopyButtons(report.longTermImprovements)" />
      </div>
    </div>

    <!-- Empty state -->
    <div v-if="!report.immediateMitigation && !report.longTermImprovements" class="text-center py-8">
      <p class="text-xs text-zinc-500">Mitigation steps will appear here once the analysis completes.</p>
    </div>
  </div>
</template>

<style scoped>
.rca-mitigation :deep(p) { margin: 0.25rem 0; line-height: 1.6; }
.rca-mitigation :deep(ul), .rca-mitigation :deep(ol) { padding-left: 1.25rem; margin: 0.25rem 0; }
.rca-mitigation :deep(li) { margin: 0.125rem 0; line-height: 1.5; }
.rca-mitigation :deep(strong) { font-weight: 600; color: var(--foreground); }
.rca-mitigation :deep(code) { font-size: 0.625rem; padding: 0.1rem 0.2rem; border-radius: 0.2rem; background: hsl(var(--secondary)); font-family: ui-monospace, monospace; }
.rca-mitigation :deep(pre) { margin: 0; padding: 0.5rem 0.625rem; border-radius: 0 0 0.375rem 0.375rem; background: hsl(var(--secondary) / 0.8); overflow-x: auto; border: 1px solid hsl(var(--border) / 0.4); border-top: 0; }
.rca-mitigation :deep(pre code) { padding: 0; background: none; font-size: 0.625rem; line-height: 1.5; }
.rca-mitigation :deep(.copy-code-btn.copied .copy-icon) { display: none; }
.rca-mitigation :deep(.copy-code-btn.copied .check-icon) { display: block; color: hsl(142 71% 45%); }
.rca-mitigation :deep(.copy-code-btn.copied .copy-label) { color: hsl(142 71% 45%); }
.rca-mitigation :deep(blockquote) { border-left: 3px solid hsl(24 100% 50%); background: hsl(24 100% 50% / 0.06); padding: 0.5rem 0.75rem; margin: 0.375rem 0; border-radius: 0.375rem; }
</style>
