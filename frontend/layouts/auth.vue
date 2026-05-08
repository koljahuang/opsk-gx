<template>
  <div class="min-h-screen flex items-center justify-center relative overflow-hidden bg-black">
    <!-- Galaxy background -->
    <img src="/bg.gif" alt="" class="absolute inset-0 w-full h-full object-cover" />

    <!-- Dark overlay for contrast -->
    <div class="absolute inset-0 bg-black/50" />

    <!-- Vignette -->
    <div class="absolute inset-0 pointer-events-none" style="background: radial-gradient(ellipse 70% 70% at 50% 50%, transparent 30%, rgba(0,0,0,0.7) 100%)" />

    <!-- Content -->
    <div class="relative z-10">
      <slot />
    </div>
  </div>
</template>
