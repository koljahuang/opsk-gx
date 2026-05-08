<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { Plus, Pencil, Trash2, X } from 'lucide-vue-next'
import { toast } from 'vue-sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
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
const authStore = useAuthStore()

interface AccountAccessEntry {
  account_id: string
  role: string
}

interface GroupMapping {
  id: string
  group_id: string
  group_name: string
  role: string
  tenant_id: string | null
  account_access: AccountAccessEntry[]
  created_at: string
  updated_at: string
}

interface Tenant {
  id: string
  name: string
  slug: string
}

interface CloudAccount {
  id: string
  provider: string
  name: string
  account_id: string
}

const mappings = ref<GroupMapping[]>([])
const tenants = ref<Tenant[]>([])
const accounts = ref<CloudAccount[]>([])
const loading = ref(true)
const saving = ref(false)

const showFormDialog = ref(false)
const editingMapping = ref<GroupMapping | null>(null)
const showDeleteDialog = ref(false)
const deletingMapping = ref<GroupMapping | null>(null)

const form = ref({
  group_id: '',
  group_name: '',
  role: 'member' as string,
  tenant_id: '' as string,
  account_access: [] as AccountAccessEntry[],
})

const isEditing = computed(() => !!editingMapping.value)

const columns = computed(() => [
  { key: 'group_name', label: t('groupMapping.groupName') },
  { key: 'group_id', label: t('groupMapping.groupId') },
  { key: 'role', label: t('groupMapping.role') },
  { key: 'tenant_id', label: t('groupMapping.tenant') },
  { key: 'account_access', label: t('groupMapping.accountAccess') },
])

async function fetchMappings() {
  loading.value = true
  try {
    mappings.value = await api.get<GroupMapping[]>('/api/entra-group-mappings')
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  } finally {
    loading.value = false
  }
}

async function fetchTenants() {
  try {
    tenants.value = await api.get<Tenant[]>('/api/tenants')
  } catch {
    // silently fail
  }
}

async function fetchAccounts() {
  try {
    accounts.value = await api.get<CloudAccount[]>('/api/accounts')
  } catch {
    // silently fail
  }
}

onMounted(() => {
  fetchMappings()
  fetchTenants()
  fetchAccounts()
})

function openCreate() {
  editingMapping.value = null
  form.value = { group_id: '', group_name: '', role: 'member', tenant_id: '', account_access: [] }
  showFormDialog.value = true
}

function openEdit(mapping: GroupMapping) {
  editingMapping.value = mapping
  form.value = {
    group_id: mapping.group_id,
    group_name: mapping.group_name,
    role: mapping.role,
    tenant_id: mapping.tenant_id || '',
    account_access: Array.isArray(mapping.account_access) ? [...mapping.account_access] : [],
  }
  showFormDialog.value = true
}

function openDelete(mapping: GroupMapping) {
  deletingMapping.value = mapping
  showDeleteDialog.value = true
}

function onRoleChange() {
  if (form.value.role === 'super_admin') {
    form.value.tenant_id = ''
  }
}

function addAccountAccess() {
  form.value.account_access.push({ account_id: '', role: 'readonly' })
}

function removeAccountAccess(index: number) {
  form.value.account_access.splice(index, 1)
}

async function saveMapping() {
  saving.value = true
  try {
    const payload = {
      group_id: form.value.group_id,
      group_name: form.value.group_name || undefined,
      role: form.value.role,
      tenant_id: form.value.role === 'super_admin' ? null : (form.value.tenant_id || null),
      account_access: form.value.account_access.filter(a => a.account_id),
    }

    if (isEditing.value) {
      await api.put(`/api/entra-group-mappings/${editingMapping.value!.id}`, payload)
      toast.success(t('groupMapping.updateSuccess'))
    } else {
      await api.post('/api/entra-group-mappings', payload)
      toast.success(t('groupMapping.createSuccess'))
    }
    showFormDialog.value = false
    await fetchMappings()
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  } finally {
    saving.value = false
  }
}

async function deleteMapping() {
  if (!deletingMapping.value) return
  try {
    await api.del(`/api/entra-group-mappings/${deletingMapping.value.id}`)
    toast.success(t('groupMapping.deleteSuccess'))
    showDeleteDialog.value = false
    deletingMapping.value = null
    await fetchMappings()
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : t('common.error')
    toast.error(msg)
  }
}

function getTenantName(tenantId: string | null): string {
  if (!tenantId) return '-'
  const tenant = tenants.value.find((t) => t.id === tenantId)
  return tenant?.name || tenantId
}

function getAccountName(accountId: string): string {
  const account = accounts.value.find((a) => a.id === accountId)
  return account ? `${account.name} (${account.account_id})` : accountId
}

function getRoleLabel(role: string): string {
  const map: Record<string, string> = {
    super_admin: t('groupMapping.superAdmin'),
    tenant_admin: t('groupMapping.tenantAdmin'),
    operator: t('groupMapping.operator'),
    member: t('groupMapping.member'),
    viewer: t('groupMapping.viewer'),
  }
  return map[role] || role
}
</script>

<template>
  <div class="space-y-4">
    <!-- Page Header -->
    <div class="flex items-center justify-between">
      <div>
        <h1 class="text-base font-semibold text-foreground">{{ t('groupMapping.title') }}</h1>
        <p class="text-xs text-muted-foreground mt-0.5">{{ t('groupMapping.syncNote') }}</p>
      </div>
      <Button v-if="authStore.isSuperAdmin" size="sm" @click="openCreate">
        <Plus class="h-3.5 w-3.5" />
        {{ t('groupMapping.create') }}
      </Button>
    </div>

    <!-- Data Table -->
    <DataTable :columns="columns" :data="mappings" :loading="loading">
      <template #cell-group_name="{ row }">
        <span class="font-medium text-foreground">{{ (row as GroupMapping).group_name || '-' }}</span>
      </template>

      <template #cell-group_id="{ row }">
        <code class="text-xs text-muted-foreground bg-muted/50 px-1.5 py-0.5 rounded">{{ (row as GroupMapping).group_id }}</code>
      </template>

      <template #cell-role="{ row }">
        <Badge :variant="{ super_admin: 'default', tenant_admin: 'info', operator: 'warning' }[(row as GroupMapping).role] || 'secondary'">
          {{ getRoleLabel((row as GroupMapping).role) }}
        </Badge>
      </template>

      <template #cell-tenant_id="{ row }">
        <span class="text-muted-foreground">{{ getTenantName((row as GroupMapping).tenant_id) }}</span>
      </template>

      <template #cell-account_access="{ row }">
        <div v-if="Array.isArray((row as GroupMapping).account_access) && (row as GroupMapping).account_access.length" class="flex flex-wrap gap-1">
          <Badge v-for="(entry, i) in (row as GroupMapping).account_access" :key="i" variant="outline" class="text-[10px]">
            {{ getAccountName(entry.account_id) }} · {{ entry.role }}
          </Badge>
        </div>
        <span v-else class="text-muted-foreground">-</span>
      </template>

      <template #actions="{ row }">
        <Button variant="ghost" size="icon-sm" @click="openEdit(row as GroupMapping)">
          <Pencil class="h-3 w-3" />
        </Button>
        <Button variant="ghost" size="icon-sm" class="text-destructive hover:text-destructive" @click="openDelete(row as GroupMapping)">
          <Trash2 class="h-3 w-3" />
        </Button>
      </template>
    </DataTable>

    <!-- Create/Edit Dialog -->
    <Dialog :open="showFormDialog" @update:open="(val) => { showFormDialog = val }">
      <DialogContent class="max-w-md">
        <DialogHeader>
          <DialogTitle>{{ isEditing ? t('groupMapping.edit') : t('groupMapping.create') }}</DialogTitle>
          <DialogDescription>{{ isEditing ? t('groupMapping.editDescription') : t('groupMapping.createDescription') }}</DialogDescription>
        </DialogHeader>

        <form class="space-y-3" @submit.prevent="saveMapping">
          <div class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('groupMapping.groupId') }} <span class="text-destructive">*</span></label>
            <Input v-model="form.group_id" :placeholder="t('groupMapping.groupIdPlaceholder')" required />
          </div>

          <div class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('groupMapping.groupName') }}</label>
            <Input v-model="form.group_name" :placeholder="t('groupMapping.groupNamePlaceholder')" />
          </div>

          <div class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('groupMapping.role') }}</label>
            <Select v-model="form.role" @update:model-value="onRoleChange">
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="super_admin">{{ t('groupMapping.superAdmin') }}</SelectItem>
                <SelectItem value="tenant_admin">{{ t('groupMapping.tenantAdmin') }}</SelectItem>
                <SelectItem value="operator">{{ t('groupMapping.operator') }}</SelectItem>
                <SelectItem value="member">{{ t('groupMapping.member') }}</SelectItem>
                <SelectItem value="viewer">{{ t('groupMapping.viewer') }}</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <div v-if="form.role !== 'super_admin'" class="space-y-1.5">
            <label class="text-xs font-medium">{{ t('groupMapping.tenant') }} <span class="text-destructive">*</span></label>
            <Select v-model="form.tenant_id">
              <SelectTrigger><SelectValue :placeholder="t('groupMapping.selectTenant')" /></SelectTrigger>
              <SelectContent>
                <SelectItem v-for="tenant in tenants" :key="tenant.id" :value="tenant.id">{{ tenant.name }}</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <!-- Account Access -->
          <div class="space-y-1.5">
            <div class="flex items-center justify-between">
              <label class="text-xs font-medium">{{ t('groupMapping.accountAccess') }}</label>
              <Button type="button" variant="ghost" size="sm" class="h-6 text-xs" @click="addAccountAccess">
                <Plus class="h-3 w-3 mr-1" />
                {{ t('groupMapping.addAccount') }}
              </Button>
            </div>
            <p class="text-[11px] text-muted-foreground">{{ t('groupMapping.accountAccessDescription') }}</p>

            <div v-for="(entry, index) in form.account_access" :key="index" class="flex items-center gap-1.5">
              <Select v-model="entry.account_id" class="flex-1">
                <SelectTrigger class="h-8 text-xs"><SelectValue :placeholder="t('groupMapping.selectAccount')" /></SelectTrigger>
                <SelectContent>
                  <SelectItem v-for="acct in accounts" :key="acct.id" :value="acct.id">
                    {{ acct.name }} ({{ acct.account_id }})
                  </SelectItem>
                </SelectContent>
              </Select>
              <Select v-model="entry.role" class="w-24">
                <SelectTrigger class="h-8 text-xs"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="admin">{{ t('groupMapping.admin') }}</SelectItem>
                  <SelectItem value="readonly">{{ t('groupMapping.readonly') }}</SelectItem>
                </SelectContent>
              </Select>
              <Button type="button" variant="ghost" size="icon-sm" @click="removeAccountAccess(index)">
                <X class="h-3 w-3" />
              </Button>
            </div>
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

    <ConfirmDialog
      :open="showDeleteDialog"
      :title="t('groupMapping.deleteTitle')"
      :description="t('groupMapping.confirmDelete')"
      :confirm-text="t('common.delete')"
      variant="destructive"
      @confirm="deleteMapping"
      @cancel="showDeleteDialog = false; deletingMapping = null"
    />
  </div>
</template>
