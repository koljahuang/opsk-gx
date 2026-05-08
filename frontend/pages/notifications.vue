<script setup lang="ts">
import { CheckCheck } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import type { AppNotification } from '@/composables/useNotifications'

definePageMeta({ middleware: ['auth'] })

const { t } = useI18n()
const router = useRouter()
const { unreadCount, notifications, fetchNotifications, markRead, markAllRead } = useNotifications()

const loading = ref(true)

onMounted(async () => {
  await fetchNotifications(100)
  loading.value = false
})

function eventBadgeVariant(type: string): string {
  if (type.includes('approved') || type.includes('executed')) return 'success'
  if (type.includes('rejected') || type.includes('failed')) return 'destructive'
  if (type.includes('submitted')) return 'info'
  return 'secondary'
}

function eventLabel(type: string): string {
  const map: Record<string, string> = {
    approval_submitted: t('notification.submitted'),
    approval_approved: t('notification.approved'),
    approval_rejected: t('notification.rejected'),
    approval_executed: t('notification.executed'),
    approval_failed: t('notification.failed'),
    approval_result_overridden: t('notification.overridden'),
  }
  return map[type] || type
}

function formatDate(dateStr: string): string {
  return new Date(dateStr).toLocaleString(undefined, {
    month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
  })
}

function handleClick(n: AppNotification) {
  if (!n.is_read) markRead(n.id)
  if (n.related_id) router.push('/approvals')
}
</script>

<template>
  <div class="space-y-4">
    <div class="flex items-center justify-between">
      <h1 class="text-base font-semibold text-foreground">{{ t('notification.title') }}</h1>
      <Button
        v-if="unreadCount > 0"
        variant="outline" size="sm"
        @click="markAllRead"
      >
        <CheckCheck class="h-3.5 w-3.5 mr-1" />
        {{ t('notification.markAllRead') }}
      </Button>
    </div>

    <!-- Loading -->
    <div v-if="loading" class="flex justify-center py-12">
      <div class="h-5 w-5 animate-spin rounded-full border-2 border-primary border-t-transparent" />
    </div>

    <!-- Empty -->
    <div v-else-if="notifications.length === 0" class="text-center py-12 text-sm text-muted-foreground">
      {{ t('notification.noNotifications') }}
    </div>

    <!-- List -->
    <div v-else class="space-y-1">
      <button
        v-for="n in notifications"
        :key="n.id"
        class="flex w-full items-start gap-3 rounded-md border border-border/40 px-4 py-3 text-left hover:bg-accent/30 transition-colors"
        :class="!n.is_read && 'bg-primary/5 border-primary/20'"
        @click="handleClick(n)"
      >
        <div class="flex-1 min-w-0 space-y-1">
          <div class="flex items-center gap-2">
            <Badge :variant="eventBadgeVariant(n.event_type) as any" class="text-[9px] h-4 px-1.5">
              {{ eventLabel(n.event_type) }}
            </Badge>
            <span v-if="!n.is_read" class="h-1.5 w-1.5 rounded-full bg-primary shrink-0" />
          </div>
          <p class="text-[12px]" :class="n.is_read ? 'text-muted-foreground' : 'text-foreground font-medium'">
            {{ n.title }}
          </p>
          <p v-if="n.description" class="text-[11px] text-muted-foreground/70 line-clamp-2">
            {{ n.description }}
          </p>
        </div>
        <span class="text-[10px] text-muted-foreground/50 shrink-0 pt-1">
          {{ formatDate(n.created_at) }}
        </span>
      </button>
    </div>
  </div>
</template>
