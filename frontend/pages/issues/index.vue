<script setup lang="ts">
import { ref, onMounted, computed, watch, nextTick } from 'vue'
import { useDebounceFn } from '@vueuse/core'
import { Search, CheckCircle2, Trash2 } from 'lucide-vue-next'
import { toast } from 'vue-sonner'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogDescription,
} from '@/components/ui/dialog'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import DataTable from '@/components/shared/DataTable.vue'
import RcaInvestigation from '@/components/issues/RcaInvestigation.vue'
import type { InvestigationStep } from '@/composables/useRcaStream'

definePageMeta({ middleware: 'auth' })

const { t } = useI18n()
const api = useApi()
const { rcaText, thinkingText, isStreaming, isComplete, error: rcaError, elapsedMs, steps, investigationSteps, parsedReport, startRca, abort, reset } = useRcaStream()

interface StoredStep {
  stepId: string
  toolName: string
  reasoning: string
  label: string
  status: string
  analysis: string
  dataText: string
  durationMs?: number
}

interface Issue {
  id: string
  title: string
  description: string | null
  source: string
  severity: 'critical' | 'high' | 'medium' | 'low'
  status: 'open' | 'investigating' | 'rca_done' | 'resolved'
  issue_type: 'incident' | 'prediction'
  rca_result: { analysis?: string; error?: string; steps?: StoredStep[] } | string | null
  rca_started_at: string | null
  rca_completed_at: string | null
  timeline: { time: string; event: string }[] | null
  created_at: string
}

const issues = ref<Issue[]>([])
const loading = ref(true)
const activeFilter = ref<string>('')
const activeTypeFilter = ref<string>('')
const autoRcaEnabled = ref(true)

const showDetailDialog = ref(false)
const selectedIssue = ref<Issue | null>(null)

const filters = [
  { value: '', label: () => t('issue.all') },
  { value: 'open', label: () => t('issue.open') },
  { value: 'investigating', label: () => t('issue.investigating') },
  { value: 'rca_done', label: () => t('issue.rcaDone') },
  { value: 'resolved', label: () => t('issue.resolved') },
]

const typeFilters = [
  { value: '', label: () => t('issue.allTypes') },
  { value: 'incident', label: () => t('issue.incident') },
  { value: 'prediction', label: () => t('issue.prediction') },
]

const filteredIssues = computed(() => {
  let result = issues.value
  if (!activeFilter.value) {
    result = result.filter((i) => i.status !== 'resolved')
  } else {
    result = result.filter((i) => i.status === activeFilter.value)
  }
  if (activeTypeFilter.value) {
    result = result.filter((i) => i.issue_type === activeTypeFilter.value)
  }
  return result
})

const columns = computed(() => [
  { key: 'title', label: t('issue.title') },
  { key: 'issue_type', label: t('issue.issueType') },
  { key: 'source', label: t('issue.source') },
  { key: 'severity', label: t('issue.severity') },
  { key: 'status', label: t('cluster.status') },
  { key: 'created_at', label: t('tenant.createdAt') },
])

// --- Helpers ---

function severityVariant(severity: string): 'destructive' | 'warning' | 'info' | 'secondary' {
  switch (severity) {
    case 'critical': return 'destructive'
    case 'high': return 'warning'
    case 'medium': return 'info'
    case 'low': return 'secondary'
    default: return 'secondary'
  }
}

function statusVariant(status: string): 'warning' | 'info' | 'success' | 'secondary' {
  switch (status) {
    case 'open': return 'warning'
    case 'investigating': return 'info'
    case 'rca_done': return 'success'
    case 'resolved': return 'secondary'
    default: return 'secondary'
  }
}

function statusLabel(status: string): string {
  const map: Record<string, string> = {
    open: t('issue.open'),
    investigating: t('issue.investigating'),
    rca_done: t('issue.rcaDone'),
    resolved: t('issue.resolved'),
  }
  return map[status] || status
}

function formatDate(dateStr: string): string {
  if (!dateStr) return '-'
  return new Date(dateStr).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}

/** Extract RCA analysis text from the stored rca_result field */
function getStoredRcaText(issue: Issue): string {
  if (!issue.rca_result) return ''
  if (typeof issue.rca_result === 'string') return issue.rca_result
  return issue.rca_result.analysis || issue.rca_result.error || ''
}

/** Extract stored investigation steps from rca_result */
function getStoredSteps(issue: Issue): InvestigationStep[] {
  if (!issue.rca_result || typeof issue.rca_result === 'string') return []
  if (!issue.rca_result.steps) return []
  return issue.rca_result.steps.map(s => ({
    stepId: s.stepId,
    toolName: s.toolName,
    reasoning: s.reasoning || '',
    label: s.label,
    status: 'complete' as const,
    dataText: s.dataText || undefined,
    analysis: s.analysis || undefined,
    durationMs: s.durationMs,
    startedAt: new Date(issue.rca_started_at || issue.created_at).getTime(),
  }))
}

/** Whether the issue has a running RCA (started but not completed) */
function isRcaRunning(issue: Issue): boolean {
  return !!issue.rca_started_at && !issue.rca_completed_at
}

/** Whether the issue has completed RCA */
function hasRcaResult(issue: Issue): boolean {
  return !!issue.rca_completed_at && !!issue.rca_result
}

/** Whether the issue can start RCA */
function canStartRca(issue: Issue): boolean {
  return issue.status !== 'resolved' && !isRcaRunning(issue)
}

// --- API ---

async function fetchIssues() {
  loading.value = true
  try {
    const params = new URLSearchParams()
    if (activeFilter.value) params.set('status', activeFilter.value)
    if (activeTypeFilter.value) params.set('issue_type', activeTypeFilter.value)
    const qs = params.toString()
    issues.value = await api.get<Issue[]>(`/api/issues${qs ? '?' + qs : ''}`)
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  } finally {
    loading.value = false
  }
}

const debouncedFetchIssues = useDebounceFn(fetchIssues, 300)

async function openDetail(issue: Issue) {
  // Refresh issue to get latest state
  try {
    const fresh = await api.get<Issue>(`/api/issues/${issue.id}`)
    selectedIssue.value = fresh
  } catch {
    selectedIssue.value = issue
  }

  reset()
  showDetailDialog.value = true

  await nextTick()
  if (!selectedIssue.value) return

  // If RCA is already running on the backend, auto-connect to the stream
  if (isRcaRunning(selectedIssue.value)) {
    try {
      const status = await api.get<{ running: boolean }>(`/api/issues/${selectedIssue.value.id}/rca/status`)
      if (status.running) {
        startRca(selectedIssue.value.id)
        return
      }
    } catch {
      // Not running — fall through
    }
  }

}

async function handleStartRca() {
  if (!selectedIssue.value) return
  reset()
  startRca(selectedIssue.value.id)
}

async function resolveIssue(issue: Issue) {
  try {
    await api.put(`/api/issues/${issue.id}`, { status: 'resolved' })
    toast.success(t('issue.resolved') || 'Issue resolved')
    await fetchIssues()
    if (selectedIssue.value?.id === issue.id) {
      showDetailDialog.value = false
    }
  } catch {
    toast.error('Failed to resolve issue')
  }
}

async function deleteIssue(issue: Issue) {
  try {
    await api.del(`/api/issues/${issue.id}`)
    toast.success(t('issue.deleted') || 'Issue deleted')
    await fetchIssues()
    if (selectedIssue.value?.id === issue.id) {
      showDetailDialog.value = false
    }
  } catch {
    toast.error('Failed to delete issue')
  }
}

// Refresh issue list when RCA completes
watch(isComplete, (done) => {
  if (done) {
    fetchIssues()
  }
})

// Close cleanup
watch(showDetailDialog, (open) => {
  if (!open) {
    if (isStreaming.value) abort()
    reset()
  }
})

async function fetchRcaConfig() {
  try {
    const res = await api.get<{ auto_rca_enabled: boolean }>('/api/issues/rca/config')
    autoRcaEnabled.value = res.auto_rca_enabled
  } catch {
    // default to true if fetch fails
  }
}

async function toggleAutoRca() {
  try {
    const newVal = !autoRcaEnabled.value
    await api.put('/api/issues/rca/config', { auto_rca_enabled: newVal })
    autoRcaEnabled.value = newVal
  } catch {
    toast.error('Failed to update RCA config')
  }
}

// Real-time refresh: watch for issue_created / rca_completed / rca_failed notifications
const { lastNotification } = useNotifications()
watch(lastNotification, (n) => {
  if (n && (n.event_type === 'issue_created' || n.event_type === 'rca_completed' || n.event_type === 'rca_failed')) {
    fetchIssues()
  }
})

onMounted(() => {
  fetchIssues()
  fetchRcaConfig()
})
</script>

<template>
  <div class="space-y-4">
    <!-- Page Header -->
    <div class="flex items-center justify-between">
      <h1 class="text-base font-semibold text-foreground">{{ t('issue.title') }}</h1>
      <button
        class="flex items-center gap-1.5 px-2 py-1 rounded-md text-[11px] font-medium transition-colors"
        :class="autoRcaEnabled
          ? 'bg-emerald-500/10 text-emerald-400 hover:bg-emerald-500/20'
          : 'bg-secondary/50 text-muted-foreground hover:bg-secondary'"
        :title="autoRcaEnabled ? 'Auto RCA enabled — click to disable' : 'Auto RCA disabled — click to enable'"
        @click="toggleAutoRca"
      >
        <span class="relative flex h-2 w-2">
          <span v-if="autoRcaEnabled" class="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75" />
          <span class="relative inline-flex rounded-full h-2 w-2" :class="autoRcaEnabled ? 'bg-emerald-400' : 'bg-muted-foreground/40'" />
        </span>
        {{ autoRcaEnabled ? 'Auto RCA' : 'Manual RCA' }}
      </button>
    </div>

    <!-- Filters -->
    <div class="flex items-center gap-3">
      <div class="flex items-center gap-1.5">
        <Button
          v-for="f in filters"
          :key="f.value"
          size="sm"
          :variant="activeFilter === f.value ? 'default' : 'outline'"
          @click="activeFilter = f.value; debouncedFetchIssues()"
        >
          {{ f.label() }}
        </Button>
      </div>

      <div class="w-px h-5 bg-border/60" />

      <div class="flex items-center gap-1.5">
        <Button
          v-for="tf in typeFilters"
          :key="tf.value"
          size="sm"
          :variant="activeTypeFilter === tf.value ? 'default' : 'outline'"
          @click="activeTypeFilter = tf.value; debouncedFetchIssues()"
        >
          {{ tf.label() }}
        </Button>
      </div>
    </div>

    <!-- Data Table -->
    <DataTable :columns="columns" :data="filteredIssues" :loading="loading">
      <template #cell-title="{ row }">
        <button class="font-medium text-foreground hover:text-primary transition-colors text-left" @click="openDetail(row as Issue)">
          {{ (row as Issue).title }}
        </button>
      </template>

      <template #cell-issue_type="{ row }">
        <Badge
          :variant="(row as Issue).issue_type === 'prediction' ? 'info' : 'warning'"
          class="text-[10px]"
        >
          {{ (row as Issue).issue_type === 'prediction' ? t('issue.prediction') : t('issue.incident') }}
        </Badge>
      </template>

      <template #cell-source="{ row }">
        <Badge variant="secondary">{{ (row as Issue).source }}</Badge>
      </template>

      <template #cell-severity="{ row }">
        <Badge :variant="severityVariant((row as Issue).severity)">
          {{ (row as Issue).severity }}
        </Badge>
      </template>

      <template #cell-status="{ row }">
        <Badge :variant="statusVariant((row as Issue).status)">
          {{ statusLabel((row as Issue).status) }}
        </Badge>
      </template>

      <template #cell-created_at="{ row }">
        <span class="text-muted-foreground">{{ formatDate((row as Issue).created_at) }}</span>
      </template>

      <template #actions="{ row }">
        <div class="flex items-center gap-0.5">
          <Tooltip>
            <TooltipTrigger as-child>
              <Button variant="ghost" size="icon-sm" @click="openDetail(row as Issue)">
                <Search class="h-3 w-3" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>{{ t('common.detail') || 'Detail' }}</TooltipContent>
          </Tooltip>
          <Tooltip v-if="(row as Issue).status !== 'resolved'">
            <TooltipTrigger as-child>
              <Button variant="ghost" size="icon-sm" class="text-emerald-400 hover:text-emerald-300" @click="resolveIssue(row as Issue)">
                <CheckCircle2 class="h-3 w-3" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>{{ t('issue.resolve') || 'Resolve' }}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger as-child>
              <Button variant="ghost" size="icon-sm" class="text-destructive hover:text-destructive/80" @click="deleteIssue(row as Issue)">
                <Trash2 class="h-3 w-3" />
              </Button>
            </TooltipTrigger>
            <TooltipContent>{{ t('common.delete') || 'Delete' }}</TooltipContent>
          </Tooltip>
        </div>
      </template>
    </DataTable>

    <!-- Detail Dialog -->
    <Dialog :open="showDetailDialog" @update:open="(val) => { showDetailDialog = val }">
      <DialogContent class="max-w-4xl max-h-[88vh] overflow-hidden flex flex-col !bg-card/80 backdrop-blur-xl border-border/40">
        <DialogHeader>
          <DialogTitle>{{ t('issue.detail') }}</DialogTitle>
          <DialogDescription class="truncate">{{ selectedIssue?.title }}</DialogDescription>
        </DialogHeader>

        <div v-if="selectedIssue" class="flex-1 overflow-y-auto space-y-3 min-h-0">
          <!-- Meta badges -->
          <div class="flex items-center gap-1.5 flex-wrap">
            <Badge
              :variant="selectedIssue.issue_type === 'prediction' ? 'info' : 'warning'"
              class="text-[10px]"
            >
              {{ selectedIssue.issue_type === 'prediction' ? t('issue.prediction') : t('issue.incident') }}
            </Badge>
            <Badge :variant="severityVariant(selectedIssue.severity)">{{ selectedIssue.severity }}</Badge>
            <Badge :variant="statusVariant(selectedIssue.status)">{{ statusLabel(selectedIssue.status) }}</Badge>
            <Badge variant="secondary">{{ selectedIssue.source }}</Badge>
          </div>

          <!-- Description -->
          <div v-if="selectedIssue.description" class="space-y-1">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('glossary.description') }}</label>
            <p class="text-xs text-foreground whitespace-pre-wrap rounded border border-border/60 bg-secondary/30 p-2">{{ selectedIssue.description }}</p>
          </div>

          <!-- RCA Investigation -->
          <RcaInvestigation
            :investigation-steps="investigationSteps"
            :legacy-steps="steps"
            :rca-text="rcaText"
            :thinking-text="thinkingText"
            :is-streaming="isStreaming"
            :is-complete="isComplete"
            :elapsed-ms="elapsedMs"
            :error="rcaError"
            :can-start="canStartRca(selectedIssue)"
            :has-stored-result="hasRcaResult(selectedIssue)"
            :stored-rca-text="getStoredRcaText(selectedIssue)"
            :stored-steps="getStoredSteps(selectedIssue)"
            :parsed-report="parsedReport"
            @start="handleStartRca"
            @abort="abort"
          />

          <!-- Timeline -->
          <div v-if="selectedIssue.timeline && selectedIssue.timeline.length > 0" class="space-y-1">
            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">{{ t('issue.timeline') }}</label>
            <div class="space-y-1 rounded border border-border/60 bg-secondary/30 p-2">
              <div
                v-for="(entry, idx) in selectedIssue.timeline"
                :key="idx"
                class="flex items-start gap-2 text-xs"
              >
                <span class="text-muted-foreground whitespace-nowrap shrink-0">{{ entry.time }}</span>
                <span class="text-foreground">{{ entry.event }}</span>
              </div>
            </div>
          </div>
        </div>

        <DialogFooter class="gap-1.5 pt-1 shrink-0">
          <Button variant="outline" size="sm" @click="showDetailDialog = false">{{ t('common.close') }}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </div>
</template>
