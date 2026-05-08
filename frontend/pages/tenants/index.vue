<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { Plus, Pencil, Trash2, Brain, Star } from 'lucide-vue-next'
import { toast } from 'vue-sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogDescription,
} from '@/components/ui/dialog'
import DataTable from '@/components/shared/DataTable.vue'
import ConfirmDialog from '@/components/shared/ConfirmDialog.vue'

definePageMeta({ middleware: 'auth' })

const { t } = useI18n()
const api = useApi()

interface Tenant {
  id: string
  name: string
  slug: string
  created_at: string
}

interface ProviderInfo {
  id: string
  name: string
  provider_type: string
  config: Record<string, unknown>
  is_default: boolean
}

const tenants = ref<Tenant[]>([])
const loading = ref(true)
const saving = ref(false)

const showFormDialog = ref(false)
const editingTenant = ref<Tenant | null>(null)
const showDeleteDialog = ref(false)
const deletingTenant = ref<Tenant | null>(null)

const form = ref({
  name: '',
})

const isEditing = computed(() => !!editingTenant.value)
const formTitle = computed(() => isEditing.value ? t('tenant.editTenant') : t('tenant.create'))

const columns = computed(() => [
  { key: 'name', label: t('tenant.name') },
  { key: 'slug', label: t('tenant.slug') },
  { key: 'models', label: t('modelCard.title') },
  { key: 'created_at', label: t('tenant.createdAt') },
])

// ─── Model Card Assignment ──────────────────────────────────────
const showModelsDialog = ref(false)
const modelsTenant = ref<Tenant | null>(null)
const allProviders = ref<ProviderInfo[]>([])
const assignedProviderIds = ref<Set<string>>(new Set())
const defaultProviderId = ref<string | null>(null)
const savingModels = ref(false)

// Track assigned model count per tenant
const tenantModelCounts = ref<Record<string, number>>({})

async function fetchTenants() {
  loading.value = true
  try {
    tenants.value = await api.get<Tenant[]>('/api/tenants')
    // Fetch model counts per tenant
    for (const t of tenants.value) {
      try {
        const data = await api.get<ProviderInfo[]>(`/api/tenants/${t.id}/providers`)
        tenantModelCounts.value[t.id] = data.length
      } catch {
        tenantModelCounts.value[t.id] = 0
      }
    }
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  } finally {
    loading.value = false
  }
}

async function fetchAllProviders() {
  try {
    const data = await api.get<ProviderInfo[]>('/api/providers')
    allProviders.value = data
  } catch { /* ignore */ }
}

onMounted(() => {
  fetchTenants()
  fetchAllProviders()
})

function formatDate(dateStr: string): string {
  if (!dateStr) return '-'
  return new Date(dateStr).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

function autoSlug(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
}

function openCreate() {
  editingTenant.value = null
  form.value = { name: '' }
  showFormDialog.value = true
}

function openEdit(tenant: Tenant) {
  editingTenant.value = tenant
  form.value = { name: tenant.name }
  showFormDialog.value = true
}

function openDelete(tenant: Tenant) {
  deletingTenant.value = tenant
  showDeleteDialog.value = true
}

async function openModels(tenant: Tenant) {
  modelsTenant.value = tenant
  assignedProviderIds.value = new Set()
  defaultProviderId.value = null

  try {
    const data = await api.get<ProviderInfo[]>(`/api/tenants/${tenant.id}/providers`)
    for (const p of data) {
      assignedProviderIds.value.add(p.id)
      if (p.is_default) defaultProviderId.value = p.id
    }
  } catch { /* ignore */ }

  showModelsDialog.value = true
}

function toggleProvider(id: string, checked: boolean) {
  const newSet = new Set(assignedProviderIds.value)
  if (checked) {
    newSet.add(id)
  } else {
    newSet.delete(id)
    if (defaultProviderId.value === id) defaultProviderId.value = null
  }
  assignedProviderIds.value = newSet
}

function setDefault(id: string) {
  defaultProviderId.value = id
  // Ensure it's assigned
  if (!assignedProviderIds.value.has(id)) {
    const newSet = new Set(assignedProviderIds.value)
    newSet.add(id)
    assignedProviderIds.value = newSet
  }
}

async function saveModels() {
  if (!modelsTenant.value) return
  savingModels.value = true
  try {
    const providerIds = Array.from(assignedProviderIds.value)
    await api.put(`/api/tenants/${modelsTenant.value.id}/providers`, {
      provider_ids: providerIds,
    })
    if (defaultProviderId.value && providerIds.includes(defaultProviderId.value)) {
      await api.put(`/api/tenants/${modelsTenant.value.id}/providers/default`, {
        provider_id: defaultProviderId.value,
      })
    }
    toast.success(t('common.success'))
    showModelsDialog.value = false
    tenantModelCounts.value[modelsTenant.value.id] = providerIds.length
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  } finally {
    savingModels.value = false
  }
}

async function saveTenant() {
  saving.value = true
  try {
    const slug = isEditing.value ? editingTenant.value!.slug : autoSlug(form.value.name)
    const payload = { name: form.value.name, slug }
    if (isEditing.value) {
      await api.put(`/api/tenants/${editingTenant.value!.id}`, payload)
      toast.success(t('tenant.updateSuccess'))
    } else {
      await api.post('/api/tenants', payload)
      toast.success(t('tenant.createSuccess'))
    }
    showFormDialog.value = false
    await fetchTenants()
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  } finally {
    saving.value = false
  }
}

async function deleteTenant() {
  if (!deletingTenant.value) return
  try {
    await api.del(`/api/tenants/${deletingTenant.value.id}`)
    toast.success(t('tenant.deleteSuccess'))
    showDeleteDialog.value = false
    deletingTenant.value = null
    await fetchTenants()
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  }
}

function getModelId(p: ProviderInfo): string {
  return (p.config?.model as string) || '-'
}
</script>

<template>
  <div class="space-y-4">
    <!-- Page Header -->
    <div class="flex items-center justify-between">
      <h1 class="text-base font-semibold text-foreground">{{ t('tenant.title') }}</h1>
      <Button size="sm" @click="openCreate">
        <Plus class="h-3.5 w-3.5" />
        {{ t('tenant.create') }}
      </Button>
    </div>

    <!-- Data Table -->
    <DataTable :columns="columns" :data="tenants" :loading="loading">
      <template #cell-name="{ row }">
        <span class="font-medium text-foreground">{{ (row as Tenant).name }}</span>
      </template>

      <template #cell-slug="{ row }">
        <code class="rounded-sm bg-secondary px-1.5 py-0.5 text-[11px] font-mono text-muted-foreground">{{ (row as Tenant).slug }}</code>
      </template>

      <template #cell-models="{ row }">
        <Button variant="ghost" size="sm" class="h-6 gap-1 text-[11px]" @click="openModels(row as Tenant)">
          <Brain class="h-3 w-3" />
          {{ tenantModelCounts[(row as Tenant).id] || 0 }}
        </Button>
      </template>

      <template #cell-created_at="{ row }">
        <span class="text-muted-foreground">{{ formatDate((row as Tenant).created_at) }}</span>
      </template>

      <template #actions="{ row }">
        <Button variant="ghost" size="icon-sm" @click="openEdit(row as Tenant)">
          <Pencil class="h-3 w-3" />
        </Button>
        <Button variant="ghost" size="icon-sm" class="text-destructive hover:text-destructive" @click="openDelete(row as Tenant)">
          <Trash2 class="h-3 w-3" />
        </Button>
      </template>
    </DataTable>

    <!-- Create/Edit Dialog -->
    <Dialog :open="showFormDialog" @update:open="(val) => { showFormDialog = val }">
      <DialogContent class="max-w-sm">
        <DialogHeader>
          <DialogTitle>{{ formTitle }}</DialogTitle>
        </DialogHeader>

        <form class="space-y-3" @submit.prevent="saveTenant">
          <div class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('tenant.name') }}</label>
            <Input v-model="form.name" :placeholder="t('tenant.name')" required />
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

    <!-- Model Card Assignment Dialog -->
    <Dialog :open="showModelsDialog" @update:open="(val) => { showModelsDialog = val }">
      <DialogContent class="max-w-md">
        <DialogHeader>
          <DialogTitle>{{ t('modelCard.assignModels') }} — {{ modelsTenant?.name }}</DialogTitle>
          <DialogDescription>{{ t('modelCard.description') }}</DialogDescription>
        </DialogHeader>

        <div v-if="allProviders.length === 0" class="py-4 text-center text-xs text-muted-foreground">
          {{ t('modelCard.noModels') }}
        </div>

        <div v-else class="space-y-1 max-h-[50vh] overflow-y-auto">
          <div
            v-for="p in allProviders"
            :key="p.id"
            class="flex items-center gap-3 rounded-md px-2.5 py-2 hover:bg-secondary/30 transition-colors"
          >
            <!-- Checkbox -->
            <Checkbox
              :checked="assignedProviderIds.has(p.id)"
              @update:checked="(val: boolean) => toggleProvider(p.id, val)"
            />

            <!-- Provider info -->
            <div class="flex-1 min-w-0">
              <div class="flex items-center gap-2">
                <span class="text-xs font-medium text-foreground truncate">{{ p.name }}</span>
                <Badge variant="secondary" class="text-[9px] shrink-0">{{ p.provider_type }}</Badge>
              </div>
              <p class="text-[10px] text-muted-foreground font-mono truncate">{{ getModelId(p) }}</p>
            </div>

            <!-- Default star -->
            <button
              type="button"
              class="shrink-0 p-0.5 rounded hover:bg-secondary/50 transition-colors"
              :class="defaultProviderId === p.id ? 'text-primary' : 'text-muted-foreground/30 hover:text-muted-foreground/60'"
              :title="t('modelCard.defaultModel')"
              @click="setDefault(p.id)"
            >
              <Star class="h-3.5 w-3.5" :class="defaultProviderId === p.id ? 'fill-primary' : ''" />
            </button>
          </div>
        </div>

        <DialogFooter class="gap-1.5 pt-2">
          <Button type="button" variant="outline" size="sm" @click="showModelsDialog = false">{{ t('common.cancel') }}</Button>
          <Button size="sm" :disabled="savingModels" @click="saveModels">
            {{ savingModels ? t('common.loading') : t('common.save') }}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>

    <ConfirmDialog
      :open="showDeleteDialog"
      :title="t('tenant.deleteTitle')"
      :description="t('tenant.confirmDelete')"
      :confirm-text="t('common.delete')"
      variant="destructive"
      @confirm="deleteTenant"
      @cancel="showDeleteDialog = false; deletingTenant = null"
    />
  </div>
</template>
