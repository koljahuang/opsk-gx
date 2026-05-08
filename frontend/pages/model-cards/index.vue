<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { Plus, Pencil, Trash2, Building2 } from 'lucide-vue-next'
import { toast } from 'vue-sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import { Switch } from '@/components/ui/switch'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogDescription,
} from '@/components/ui/dialog'
import ConfirmDialog from '@/components/shared/ConfirmDialog.vue'

definePageMeta({ middleware: ['auth', 'admin'] })

const { t } = useI18n()
const api = useApi()

interface Provider {
  id: string
  name: string
  provider_type: string
  config: Record<string, unknown>
  is_default: boolean
  created_at: string
  updated_at: string
}

interface ProviderType {
  value: string
  label: string
}

const providers = ref<Provider[]>([])
const providerTypes = ref<ProviderType[]>([])
const tenantCounts = ref<Record<string, number>>({})
const loading = ref(true)
const saving = ref(false)

const showFormDialog = ref(false)
const editingProvider = ref<Provider | null>(null)
const showDeleteDialog = ref(false)
const deletingProvider = ref<Provider | null>(null)

const form = ref({
  name: '',
  provider_type: '',
  model: '',
  api_key: '',
  base_url: '',
  max_turns: 25,
  timeout: 120000,
  permission_mode: 'readonly',
})

function permBadgeVariant(mode: unknown): 'destructive' | 'secondary' | 'outline' {
  if (mode === 'bypassPermissions') return 'destructive'
  if (mode === 'readwrite') return 'secondary'
  return 'outline'
}

function permBadgeLabel(mode: unknown): string {
  if (mode === 'bypassPermissions') return t('modelCard.permissionBypass')
  if (mode === 'readwrite') return t('modelCard.permissionReadWrite')
  return t('modelCard.permissionDefault')
}

// Model presets per provider type
const bedrockModels = [
  { id: 'us.anthropic.claude-sonnet-4-6', label: 'Claude Sonnet 4.6' },
  { id: 'us.anthropic.claude-opus-4-6-v1', label: 'Claude Opus 4.6' },
  { id: 'us.anthropic.claude-haiku-4-5-20251001-v1:0', label: 'Claude Haiku 4.5' },
]

const showModelDropdown = ref(false)

const isEditing = computed(() => !!editingProvider.value)
const formTitle = computed(() => isEditing.value ? t('modelCard.editModel') : t('modelCard.addModel'))

const isBedrock = computed(() => form.value.provider_type === 'bedrock')
const isGateway = computed(() => form.value.provider_type === 'gateway')

const currentModelPresets = computed(() => {
  if (isBedrock.value) return bedrockModels
  return []
})

async function fetchProviders() {
  loading.value = true
  try {
    providers.value = await api.get<Provider[]>('/api/providers')
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  } finally {
    loading.value = false
  }
}

async function fetchTypes() {
  try {
    providerTypes.value = await api.get<ProviderType[]>('/api/providers/types')
  } catch { /* ignore */ }
}

async function fetchTenantCounts() {
  try {
    const data = await api.get<[string, number][]>('/api/providers/assignments')
    const map: Record<string, number> = {}
    for (const [pid, count] of data) map[pid] = count
    tenantCounts.value = map
  } catch { /* ignore */ }
}

onMounted(() => {
  fetchProviders()
  fetchTypes()
  fetchTenantCounts()
})

function getProviderLabel(type: string): string {
  const pt = providerTypes.value.find((p) => p.value === type)
  return pt?.label || type
}

function getModelId(p: Provider): string {
  return (p.config.model as string) || '-'
}

function getTenantCount(p: Provider): number {
  return tenantCounts.value[p.id] || 0
}

function openCreate() {
  editingProvider.value = null
  const defaultType = providerTypes.value[0]?.value || 'bedrock'
  form.value = {
    name: '',
    provider_type: defaultType,
    model: '',
    api_key: '',
    base_url: '',
    max_turns: 25,
    timeout: 120000,
    permission_mode: 'readonly',
  }
  showFormDialog.value = true
}

function openEdit(p: Provider) {
  editingProvider.value = p
  form.value = {
    name: p.name,
    provider_type: p.provider_type,
    model: (p.config.model as string) || '',
    api_key: (p.config.api_key as string) || '',
    base_url: (p.config.base_url as string) || '',
    max_turns: (p.config.max_turns as number) ?? 25,
    timeout: (p.config.timeout as number) ?? 120000,
    permission_mode: (p.config.permission_mode as string) || 'readonly',
  }
  showFormDialog.value = true
}

function openDelete(p: Provider) {
  deletingProvider.value = p
  showDeleteDialog.value = true
}

function selectModel(modelId: string) {
  form.value.model = modelId
  showModelDropdown.value = false
}

// Close model dropdown when clicking outside
function onModelInputBlur() {
  setTimeout(() => { showModelDropdown.value = false }, 150)
}

async function saveProvider() {
  saving.value = true
  try {
    const config: Record<string, unknown> = {
      model: form.value.model || null,
      max_turns: form.value.max_turns,
      timeout: form.value.timeout,
      permission_mode: form.value.permission_mode,
    }
    if (isGateway.value) {
      if (form.value.api_key) config.api_key = form.value.api_key
      if (form.value.base_url) config.base_url = form.value.base_url
    }

    if (isEditing.value) {
      await api.put(`/api/providers/${editingProvider.value!.id}`, {
        name: form.value.name,
        provider_type: form.value.provider_type,
        config,
      })
    } else {
      await api.post('/api/providers', {
        name: form.value.name,
        provider_type: form.value.provider_type,
        config,
      })
    }
    toast.success(t('common.success'))
    showFormDialog.value = false
    await fetchProviders()
    await fetchTenantCounts()
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  } finally {
    saving.value = false
  }
}

async function deleteProvider() {
  if (!deletingProvider.value) return
  try {
    await api.del(`/api/providers/${deletingProvider.value.id}`)
    toast.success(t('common.success'))
    showDeleteDialog.value = false
    deletingProvider.value = null
    await fetchProviders()
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  }
}

// ─── Tenant Assignment ────────────────────────────────────────────

interface Tenant {
  id: string
  name: string
  slug: string
}

interface TenantAssignment {
  tenant_id: string
  tenant_name: string
  is_default: boolean
}

const allTenants = ref<Tenant[]>([])
const showTenantsDialog = ref(false)
const assigningProvider = ref<Provider | null>(null)
const assignedTenantIds = ref<string[]>([])
const savingTenants = ref(false)

async function fetchAllTenants() {
  try {
    allTenants.value = await api.get<Tenant[]>('/api/tenants')
  } catch { /* ignore */ }
}

onMounted(() => {
  fetchAllTenants()
})

function isTenantAssigned(id: string): boolean {
  return assignedTenantIds.value.includes(id)
}

async function openTenantAssign(p: Provider) {
  assigningProvider.value = p
  assignedTenantIds.value = []

  try {
    const data = await api.get<TenantAssignment[]>(`/api/providers/${p.id}/tenants`)
    assignedTenantIds.value = data.map(a => a.tenant_id)
  } catch { /* ignore */ }

  showTenantsDialog.value = true
}

function toggleTenant(id: string) {
  if (assignedTenantIds.value.includes(id)) {
    assignedTenantIds.value = assignedTenantIds.value.filter(t => t !== id)
  } else {
    assignedTenantIds.value = [...assignedTenantIds.value, id]
  }
}

async function saveTenantAssignment() {
  if (!assigningProvider.value) return
  savingTenants.value = true
  const payload = {
    tenant_ids: [...assignedTenantIds.value],
  }
  console.log('[model-cards] saving tenant assignment:', assigningProvider.value.id, payload)
  try {
    const res = await api.put(`/api/providers/${assigningProvider.value.id}/tenants`, payload)
    console.log('[model-cards] save response:', res)
    toast.success(t('common.success'))
    showTenantsDialog.value = false
    await fetchTenantCounts()
  } catch (err: unknown) {
    console.error('[model-cards] save error:', err)
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  } finally {
    savingTenants.value = false
  }
}
</script>

<template>
  <div class="space-y-4">
    <!-- Page Header -->
    <div class="flex items-center justify-between">
      <div>
        <h1 class="text-base font-semibold text-foreground">{{ t('modelCard.title') }}</h1>
        <p class="text-xs text-muted-foreground mt-0.5">{{ t('modelCard.description') }}</p>
      </div>
      <Button size="sm" @click="openCreate">
        <Plus class="h-3.5 w-3.5" />
        {{ t('modelCard.addModel') }}
      </Button>
    </div>

    <!-- Loading -->
    <div v-if="loading" class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
      <div v-for="i in 3" :key="i" class="rounded-lg border border-border/60 bg-card p-4">
        <div class="space-y-2">
          <div class="h-4 w-24 animate-pulse rounded bg-secondary/50" />
          <div class="h-3 w-40 animate-pulse rounded bg-secondary/50" />
        </div>
      </div>
    </div>

    <!-- Empty State -->
    <div v-else-if="providers.length === 0" class="rounded-lg border border-dashed border-border/60 bg-card/50 p-8 text-center">
      <p class="text-xs text-muted-foreground">{{ t('modelCard.noModels') }}</p>
      <Button size="sm" class="mt-3" @click="openCreate">
        <Plus class="h-3.5 w-3.5" />
        {{ t('modelCard.addModel') }}
      </Button>
    </div>

    <!-- Model Cards -->
    <div v-else class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
      <div
        v-for="p in providers"
        :key="p.id"
        class="group rounded-lg border border-border/60 bg-card p-4 hover:border-border transition-colors relative"
      >
        <!-- Name + provider badge -->
        <div class="flex items-center gap-2 mb-2">
          <span class="text-sm font-medium text-foreground truncate">{{ p.name }}</span>
          <Badge variant="secondary" class="text-[10px] shrink-0">{{ getProviderLabel(p.provider_type) }}</Badge>
        </div>

        <!-- Model ID -->
        <p class="text-xs text-muted-foreground font-mono truncate mb-3">{{ getModelId(p) }}</p>

        <!-- Meta row -->
        <div class="flex items-center justify-between">
          <div class="flex items-center gap-2 text-[10px] text-muted-foreground/60">
            <Badge
              :variant="permBadgeVariant(p.config.permission_mode)"
              class="text-[9px] px-1.5 py-0"
            >{{ permBadgeLabel(p.config.permission_mode) }}</Badge>
            <Button variant="outline" size="sm" class="h-5 px-1.5 text-[10px] gap-1" @click="openTenantAssign(p)">
              <Building2 class="h-3 w-3" />
              {{ getTenantCount(p) }} {{ getTenantCount(p) === 1 ? 'tenant' : 'tenants' }}
            </Button>
          </div>
          <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
            <Button variant="ghost" size="icon-sm" @click="openEdit(p)">
              <Pencil class="h-3 w-3" />
            </Button>
            <Button variant="ghost" size="icon-sm" class="text-destructive hover:text-destructive" @click="openDelete(p)">
              <Trash2 class="h-3 w-3" />
            </Button>
          </div>
        </div>
      </div>
    </div>

    <!-- Create/Edit Dialog -->
    <Dialog :open="showFormDialog" @update:open="(val) => { showFormDialog = val }">
      <DialogContent class="max-w-sm">
        <DialogHeader>
          <DialogTitle>{{ formTitle }}</DialogTitle>
          <DialogDescription>{{ formTitle }}</DialogDescription>
        </DialogHeader>

        <form class="space-y-3" @submit.prevent="saveProvider">
          <!-- Name -->
          <div class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('modelCard.name') }}</label>
            <Input v-model="form.name" :placeholder="t('modelCard.namePlaceholder')" required />
          </div>

          <!-- Provider Type -->
          <div class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('modelCard.providerType') }}</label>
            <Select v-model="form.provider_type">
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem v-for="pt in providerTypes" :key="pt.value" :value="pt.value">{{ pt.label }}</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <!-- Model (Combobox-like: input + dropdown) -->
          <div class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('modelCard.model') }}</label>
            <div class="relative">
              <Input
                v-model="form.model"
                :placeholder="isBedrock ? 'us.anthropic.claude-sonnet-4-6' : 'claude-sonnet-4-6'"
                @focus="showModelDropdown = currentModelPresets.length > 0"
                @blur="onModelInputBlur"
              />
              <!-- Preset dropdown -->
              <div
                v-if="showModelDropdown && currentModelPresets.length > 0"
                class="absolute z-50 top-full left-0 right-0 mt-1 rounded border border-border/60 bg-popover shadow-md overflow-hidden"
              >
                <button
                  v-for="preset in currentModelPresets"
                  :key="preset.id"
                  type="button"
                  class="flex items-center justify-between w-full px-2.5 py-1.5 text-xs hover:bg-secondary/50 transition-colors text-left"
                  @mousedown.prevent="selectModel(preset.id)"
                >
                  <span class="font-mono text-foreground">{{ preset.id }}</span>
                  <span class="text-muted-foreground/60 ml-2 text-[10px]">{{ preset.label }}</span>
                </button>
              </div>
            </div>
            <p class="text-[10px] text-muted-foreground/60">
              {{ isBedrock ? t('modelCard.modelHintBedrock') : isGateway ? t('modelCard.modelHintGateway') : t('modelCard.modelHint') }}
            </p>
          </div>

          <!-- Gateway: Base URL -->
          <div v-if="isGateway" class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('modelCard.baseUrl') }}</label>
            <Input v-model="form.base_url" placeholder="https://..." />
          </div>

          <!-- Gateway: API Key -->
          <div v-if="isGateway" class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('modelCard.apiKey') }}</label>
            <Input v-model="form.api_key" type="password" placeholder="sk-..." />
          </div>

          <!-- Max Turns + Timeout -->
          <div class="grid grid-cols-2 gap-3">
            <div class="space-y-1.5">
              <label class="text-xs font-medium">{{ t('modelCard.maxTurns') }}</label>
              <Input v-model.number="form.max_turns" type="number" />
            </div>
            <div class="space-y-1.5">
              <label class="text-xs font-medium">{{ t('modelCard.timeout') }}</label>
              <Input v-model.number="form.timeout" type="number" />
            </div>
          </div>

          <!-- Permission Mode -->
          <div class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('modelCard.permissionMode') }}</label>
            <Select v-model="form.permission_mode">
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="readonly">{{ t('modelCard.permissionDefault') }}</SelectItem>
                <SelectItem value="readwrite">{{ t('modelCard.permissionReadWrite') }}</SelectItem>
                <SelectItem value="bypassPermissions">{{ t('modelCard.permissionBypass') }}</SelectItem>
              </SelectContent>
            </Select>
            <p class="text-[10px] text-muted-foreground/60">{{ t('modelCard.permissionHint') }}</p>
          </div>

          <DialogFooter class="gap-1.5 pt-1">
            <Button type="button" variant="outline" size="sm" @click="showFormDialog = false">{{ t('common.cancel') }}</Button>
            <Button type="submit" size="sm" :disabled="saving">
              {{ saving ? t('common.loading') : (isEditing ? t('common.save') : t('common.create')) }}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>

    <!-- Tenant Assignment Dialog -->
    <Dialog :open="showTenantsDialog" @update:open="(val) => { showTenantsDialog = val }">
      <DialogContent class="max-w-sm">
        <DialogHeader>
          <DialogTitle>{{ t('modelCard.assignToTenants') }}</DialogTitle>
          <DialogDescription>{{ assigningProvider?.name }} — {{ t('modelCard.assignToTenantsHint') }}</DialogDescription>
        </DialogHeader>

        <div v-if="allTenants.length === 0" class="py-4 text-center text-xs text-muted-foreground">
          {{ t('tenant.noTenants') }}
        </div>

        <div v-else class="space-y-0.5 max-h-[50vh] overflow-y-auto -mx-1">
          <button
            v-for="tenant in allTenants"
            :key="tenant.id"
            type="button"
            class="flex items-center justify-between w-full rounded-md px-3 py-2.5 hover:bg-secondary/40 transition-colors cursor-pointer"
            @click="toggleTenant(tenant.id)"
          >
            <div class="flex items-center gap-2 min-w-0">
              <span class="text-xs font-medium text-foreground">{{ tenant.name }}</span>
              <code class="rounded bg-secondary/60 px-1.5 py-0.5 text-[10px] font-mono text-muted-foreground">{{ tenant.slug }}</code>
            </div>
            <Switch
              :checked="isTenantAssigned(tenant.id)"
              class="pointer-events-none"
            />
          </button>
        </div>

        <div v-if="assignedTenantIds.length > 0" class="text-[10px] text-muted-foreground/60 pt-1">
          {{ assignedTenantIds.length }} {{ assignedTenantIds.length === 1 ? 'tenant' : 'tenants' }} selected
        </div>

        <DialogFooter class="gap-1.5 pt-1">
          <Button type="button" variant="outline" size="sm" @click="showTenantsDialog = false">{{ t('common.cancel') }}</Button>
          <Button size="sm" :disabled="savingTenants" @click="saveTenantAssignment">
            {{ savingTenants ? t('common.loading') : t('common.save') }}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>

    <ConfirmDialog
      :open="showDeleteDialog"
      :title="t('common.delete')"
      :description="t('modelCard.confirmDelete')"
      :confirm-text="t('common.delete')"
      variant="destructive"
      @confirm="deleteProvider"
      @cancel="showDeleteDialog = false; deletingProvider = null"
    />
  </div>
</template>
