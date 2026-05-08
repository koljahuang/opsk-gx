<script setup lang="ts">
import { Bell, Check, CheckCheck, ClipboardCheck, XCircle, AlertTriangle, Loader2, Undo2, Rocket, ShieldAlert } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import { Popover, PopoverTrigger, PopoverContent } from '@/components/ui/popover'
import { ScrollArea } from '@/components/ui/scroll-area'
import type { AppNotification } from '@/composables/useNotifications'

const { t } = useI18n()
const router = useRouter()
const { unreadCount, notifications, loaded, fetchNotifications, markRead, markAllRead, connect, disconnect } = useNotifications()

const popoverOpen = ref(false)

onMounted(() => {
  connect()
})

onUnmounted(() => {
  disconnect()
})

// Lazy-load notifications on first popover open
watch(popoverOpen, (open) => {
  if (open && !loaded.value) {
    fetchNotifications()
  } else if (open) {
    fetchNotifications()
  }
})

function eventIcon(type: string) {
  switch (type) {
    case 'approval_submitted': return ClipboardCheck
    case 'approval_approved': return Check
    case 'approval_rejected': return XCircle
    case 'approval_executed': return CheckCheck
    case 'approval_failed': return AlertTriangle
    case 'approval_result_overridden': return Loader2
    case 'approval_withdrawn': return Undo2
    case 'issue_created': return ShieldAlert
    case 'rca_completed': return Check
    case 'rca_failed': return AlertTriangle
    case 'deployment_change': return Rocket
    default: return Bell
  }
}

function eventColor(type: string): string {
  switch (type) {
    case 'approval_approved':
    case 'approval_executed':
      return 'text-emerald-500'
    case 'approval_rejected':
    case 'approval_failed':
      return 'text-red-400'
    case 'approval_submitted':
      return 'text-blue-400'
    case 'approval_withdrawn':
      return 'text-amber-400'
    case 'issue_created':
      return 'text-orange-400'
    case 'rca_completed':
      return 'text-emerald-500'
    case 'rca_failed':
      return 'text-red-400'
    case 'deployment_change':
      return 'text-cyan-400'
    default:
      return 'text-muted-foreground'
  }
}

function timeAgo(dateStr: string): string {
  const diff = Date.now() - new Date(dateStr).getTime()
  const mins = Math.floor(diff / 60000)
  if (mins < 1) return t('notification.justNow')
  if (mins < 60) return `${mins}m`
  const hours = Math.floor(mins / 60)
  if (hours < 24) return `${hours}h`
  const days = Math.floor(hours / 24)
  return `${days}d`
}

function handleClick(n: AppNotification) {
  if (!n.is_read) markRead(n.id)
  popoverOpen.value = false
  if (n.event_type === 'issue_created' || n.event_type === 'rca_completed' || n.event_type === 'rca_failed') {
    router.push('/issues')
  } else if (n.event_type === 'deployment_change') {
    router.push('/deployments')
  } else if (n.related_id) {
    router.push('/approvals')
  }
}

function handleMarkAllRead() {
  markAllRead()
}
</script>

<template>
  <ClientOnly>
    <Popover v-model:open="popoverOpen">
      <PopoverTrigger as-child>
        <Button variant="ghost" size="sm" class="relative h-8 w-8 p-0 text-muted-foreground hover:text-foreground">
          <Bell class="h-4 w-4" />
          <span
            v-if="unreadCount > 0"
            class="absolute -top-0.5 -right-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-red-500 px-1 text-[9px] font-bold text-white shadow-[0_0_6px_rgba(239,68,68,0.6)] animate-pulse"
          >
            {{ unreadCount > 99 ? '99+' : unreadCount }}
          </span>
        </Button>
      </PopoverTrigger>
      <PopoverContent class="w-80 p-0" align="end" :side-offset="8">
        <!-- Header -->
        <div class="flex items-center justify-between border-b border-border/60 px-3 py-2">
          <span class="text-xs font-semibold">{{ t('notification.title') }}</span>
          <Button
            v-if="unreadCount > 0"
            variant="ghost" size="sm"
            class="h-5 text-[10px] px-1.5 text-muted-foreground"
            @click="handleMarkAllRead"
          >
            {{ t('notification.markAllRead') }}
          </Button>
        </div>

        <!-- Notification list -->
        <ScrollArea class="max-h-80">
          <div v-if="notifications.length === 0" class="flex items-center justify-center py-8 text-xs text-muted-foreground">
            {{ t('notification.noNotifications') }}
          </div>
          <div v-else class="divide-y divide-border/40">
            <button
              v-for="n in notifications.slice(0, 20)"
              :key="n.id"
              class="flex w-full items-start gap-2.5 px-3 py-2.5 text-left hover:bg-accent/50 transition-colors"
              :class="!n.is_read && 'bg-primary/5'"
              @click="handleClick(n)"
            >
              <component
                :is="eventIcon(n.event_type)"
                class="h-4 w-4 mt-0.5 shrink-0"
                :class="eventColor(n.event_type)"
              />
              <div class="flex-1 min-w-0">
                <p class="text-[11px] leading-tight" :class="n.is_read ? 'text-muted-foreground' : 'text-foreground font-medium'">
                  {{ n.title }}
                </p>
                <p v-if="n.description" class="text-[10px] text-muted-foreground/70 truncate mt-0.5">
                  {{ n.description }}
                </p>
              </div>
              <div class="flex items-center gap-1 shrink-0">
                <span class="text-[9px] text-muted-foreground/50">{{ timeAgo(n.created_at) }}</span>
                <span v-if="!n.is_read" class="h-1.5 w-1.5 rounded-full bg-primary" />
              </div>
            </button>
          </div>
        </ScrollArea>

        <!-- Footer -->
        <div class="border-t border-border/60 px-3 py-1.5">
          <NuxtLink
            to="/notifications"
            class="block text-center text-[10px] text-primary hover:underline"
            @click="popoverOpen = false"
          >
            {{ t('notification.viewAll') }}
          </NuxtLink>
        </div>
      </PopoverContent>
    </Popover>
  </ClientOnly>
</template>
