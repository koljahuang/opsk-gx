<script setup lang="ts">
import { Sonner } from '@/components/ui/sonner'
</script>

<template>
  <NuxtLoadingIndicator
    color="linear-gradient(90deg, #FF6600 0%, #22c55e 40%, #3b82f6 70%, #FF6600 100%)"
    :height="3"
    :duration="5000"
    :throttle="0"
  />
  <NuxtLayout>
    <NuxtPage />
  </NuxtLayout>
  <Sonner />
</template>
