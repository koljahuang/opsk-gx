<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'

definePageMeta({ layout: 'auth' })

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const api = useApi()

const token = computed(() => route.query.token as string)
const email = ref('')
const password = ref('')
const confirmPassword = ref('')
const error = ref('')
const loading = ref(false)
const validating = ref(true)
const expired = ref(false)
const success = ref(false)

onMounted(async () => {
  if (!token.value) {
    error.value = t('invite.invalidToken')
    validating.value = false
    return
  }

  try {
    const result = await api.get<{ email: string }>(`/api/auth/invite/${token.value}`)
    email.value = result.email
  } catch (e: any) {
    if (e.message?.includes('expired')) {
      expired.value = true
    } else {
      error.value = e.message || t('invite.invalidToken')
    }
  } finally {
    validating.value = false
  }
})

async function handleSubmit() {
  error.value = ''

  if (password.value.length < 8) {
    error.value = t('invite.passwordMinLength')
    return
  }

  if (password.value !== confirmPassword.value) {
    error.value = t('invite.passwordMismatch')
    return
  }

  loading.value = true
  try {
    await api.post(`/api/auth/invite/${token.value}/redeem`, {
      password: password.value,
    })
    success.value = true
    setTimeout(() => router.push('/login'), 2000)
  } catch (e: any) {
    error.value = e.message || t('common.error')
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <div class="w-full max-w-[420px] space-y-8 relative z-10 px-4">
    <!-- Logo + header -->
    <div class="text-center space-y-3">
      <img src="/logo-icon.png" alt="Ops K" class="mx-auto h-28 w-auto drop-shadow-2xl" />
      <h1 class="text-2xl font-bold text-white">{{ t('invite.title') }}</h1>
      <p class="text-sm text-white/90">{{ t('invite.description') }}</p>
    </div>

    <!-- Card -->
    <div class="rounded-2xl border border-white/[0.04] bg-white/[0.015] backdrop-blur-sm p-9 space-y-6">

      <!-- Loading state -->
      <div v-if="validating" class="text-center py-8">
        <p class="text-sm text-white/60">{{ t('common.loading') }}</p>
      </div>

      <!-- Expired -->
      <div v-else-if="expired" class="text-center py-4 space-y-4">
        <p class="text-sm text-red-400/80">{{ t('invite.expired') }}</p>
        <Button
          variant="outline"
          class="rounded-xl border-white/[0.05] bg-white/[0.02] hover:bg-white/[0.05] text-white/90"
          @click="router.push('/login')"
        >
          {{ t('auth.backToLogin') }}
        </Button>
      </div>

      <!-- Success -->
      <div v-else-if="success" class="text-center py-4 space-y-4">
        <p class="text-sm text-green-400">{{ t('invite.success') }}</p>
        <p class="text-xs text-white/50">{{ t('invite.redirecting') }}</p>
      </div>

      <!-- Error (invalid token) -->
      <div v-else-if="error && !email" class="text-center py-4 space-y-4">
        <p class="text-sm text-red-400/80">{{ error }}</p>
        <Button
          variant="outline"
          class="rounded-xl border-white/[0.05] bg-white/[0.02] hover:bg-white/[0.05] text-white/90"
          @click="router.push('/login')"
        >
          {{ t('auth.backToLogin') }}
        </Button>
      </div>

      <!-- Set password form -->
      <form v-else class="space-y-5" @submit.prevent="handleSubmit">
        <div class="space-y-2">
          <label class="text-[11px] font-medium text-white/90 uppercase tracking-[0.15em]">{{ t('user.email') }}</label>
          <Input
            :model-value="email"
            disabled
            class="h-11 rounded-xl border-white/[0.05] bg-white/[0.02] px-4 text-sm text-white/50"
          />
        </div>

        <div class="space-y-2">
          <label class="text-[11px] font-medium text-white/90 uppercase tracking-[0.15em]">{{ t('invite.setPassword') }}</label>
          <Input
            v-model="password"
            type="password"
            required
            autocomplete="new-password"
            :placeholder="t('auth.newPassword')"
            class="h-11 rounded-xl border-white/[0.05] bg-white/[0.02] px-4 text-sm text-white placeholder:text-white/30 focus:border-primary/30 focus:bg-white/[0.04] transition-all duration-200"
          />
        </div>

        <div class="space-y-2">
          <label class="text-[11px] font-medium text-white/90 uppercase tracking-[0.15em]">{{ t('invite.confirmPassword') }}</label>
          <Input
            v-model="confirmPassword"
            type="password"
            required
            autocomplete="new-password"
            :placeholder="t('auth.confirmPassword')"
            class="h-11 rounded-xl border-white/[0.05] bg-white/[0.02] px-4 text-sm text-white placeholder:text-white/30 focus:border-primary/30 focus:bg-white/[0.04] transition-all duration-200"
          />
        </div>

        <p v-if="error" class="text-xs text-red-400/80 bg-red-500/5 border border-red-500/10 rounded-lg px-3 py-2">{{ error }}</p>

        <Button
          type="submit"
          :disabled="loading"
          class="w-full h-11 rounded-xl text-sm font-semibold text-white bg-gradient-to-r from-primary to-orange-500 hover:brightness-110 shadow-lg shadow-primary/20 hover:shadow-primary/35 transition-all duration-200 active:scale-[0.97]"
        >
          {{ loading ? t('common.loading') : t('invite.setPassword') }}
        </Button>
      </form>
    </div>

    <!-- Bottom toggles -->
    <div class="flex justify-center gap-2 opacity-30 hover:opacity-70 transition-opacity">
      <LayoutThemeToggle />
      <LayoutLangSwitch />
    </div>
  </div>
</template>
