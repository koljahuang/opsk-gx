<script setup lang="ts">
import { Handle, Position } from '@vue-flow/core'
import { Globe, Unplug, Box, Rocket, Container, Server } from 'lucide-vue-next'

const props = defineProps<{
  data: {
    label: string
    subtitle?: string
    kind: string        // "ingress" | "service" | "deployment" | "rollout" | "pod" | "node"
    status: string      // "healthy" | "warning" | "critical" | "unknown"
    replicas?: string
    namespace: string
    cluster: string
    cpu?: string
    memory?: string
    ip?: string
    containers?: number
  }
}>()

const kindConfig: Record<string, { icon: any; border: string; bg: string; text: string }> = {
  ingress:    { icon: Globe,     border: 'border-purple-500/40', bg: 'bg-purple-500/8', text: 'text-purple-400' },
  service:    { icon: Unplug,    border: 'border-blue-500/40',   bg: 'bg-blue-500/8',   text: 'text-blue-400' },
  deployment: { icon: Box,       border: 'border-emerald-500/40', bg: 'bg-emerald-500/8', text: 'text-emerald-400' },
  rollout:    { icon: Rocket,    border: 'border-orange-500/40', bg: 'bg-orange-500/8', text: 'text-orange-400' },
  pod:        { icon: Container, border: 'border-cyan-500/40',   bg: 'bg-cyan-500/8',   text: 'text-cyan-400' },
  node:       { icon: Server,    border: 'border-pink-500/40',   bg: 'bg-pink-500/8',   text: 'text-pink-400' },
}

const statusDot: Record<string, string> = {
  healthy:  'bg-emerald-400 shadow-emerald-400/60',
  warning:  'bg-amber-400 shadow-amber-400/60',
  critical: 'bg-red-400 shadow-red-400/60 animate-pulse',
  unknown:  'bg-zinc-500 shadow-zinc-500/40',
}

const config = computed(() => kindConfig[props.data.kind] || kindConfig.deployment)
const dot = computed(() => statusDot[props.data.status] || statusDot.unknown)
</script>

<template>
  <div
    class="group relative rounded-lg border px-3 py-2 min-w-[130px] max-w-[180px] transition-all duration-300 hover:scale-105 cursor-pointer shadow-lg"
    :class="[config.border, config.bg]"
    style="background: rgba(17, 18, 23, 0.92); backdrop-filter: blur(8px);"
  >
    <!-- Handles -->
    <Handle type="target" :position="Position.Left" class="!w-2 !h-2 !bg-white/20 !border-white/30" />
    <Handle type="source" :position="Position.Right" class="!w-2 !h-2 !bg-white/20 !border-white/30" />
    <Handle type="target" :position="Position.Top" class="!w-2 !h-2 !bg-white/20 !border-white/30" />
    <Handle type="source" :position="Position.Bottom" class="!w-2 !h-2 !bg-white/20 !border-white/30" />

    <!-- Status dot -->
    <div
      class="absolute -top-1 -right-1 h-2.5 w-2.5 rounded-full shadow-[0_0_6px]"
      :class="dot"
    />

    <!-- Content -->
    <div class="flex items-center gap-2">
      <div class="shrink-0 rounded-md p-1" :class="config.bg">
        <component :is="config.icon" class="h-3.5 w-3.5" :class="config.text" />
      </div>
      <div class="min-w-0 flex-1">
        <div class="text-[11px] font-semibold text-foreground truncate leading-tight">{{ data.label }}</div>
        <div v-if="data.subtitle" class="text-[9px] text-muted-foreground/60 truncate leading-tight">{{ data.subtitle }}</div>
      </div>
    </div>

    <!-- Replicas tag -->
    <div v-if="data.replicas" class="mt-1 text-[8px] font-mono px-1 py-0.5 rounded bg-white/5 text-muted-foreground/50 text-center tabular-nums">
      {{ data.replicas }}
    </div>

    <!-- Node capacity (cpu/memory) -->
    <div v-if="data.kind === 'node' && (data.cpu || data.memory)" class="mt-1 flex items-center gap-1.5 text-[8px] font-mono text-muted-foreground/50">
      <span v-if="data.cpu">{{ data.cpu }} cpu</span>
      <span v-if="data.cpu && data.memory" class="text-muted-foreground/20">·</span>
      <span v-if="data.memory">{{ data.memory }}</span>
    </div>

    <!-- Pod IP -->
    <div v-if="data.kind === 'pod' && data.ip" class="mt-1 text-[8px] font-mono px-1 py-0.5 rounded bg-white/5 text-muted-foreground/40 text-center tabular-nums">
      {{ data.ip }}
    </div>
  </div>
</template>
