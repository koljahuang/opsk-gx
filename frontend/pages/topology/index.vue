<script setup lang="ts">
import { markRaw } from 'vue'
import { VueFlow, useVueFlow } from '@vue-flow/core'
import { MiniMap } from '@vue-flow/minimap'
import { Controls } from '@vue-flow/controls'
import { Network, RefreshCw, Loader2, Clock } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import TopologyNode from '@/components/topology/TopologyNode.vue'

import '@vue-flow/core/dist/style.css'
import '@vue-flow/core/dist/theme-default.css'
import '@vue-flow/minimap/dist/style.css'
import '@vue-flow/controls/dist/style.css'

definePageMeta({ middleware: 'auth' })

const { t } = useI18n()
const api = useApi()

// ─── Types ──────────────────────────────────────────────────────
interface TopoNode {
  id: string
  label: string
  subtitle: string | null
  kind: string
  namespace: string
  cluster: string
  cluster_id: string
  status: string
  replicas: string | null
  node_name?: string
  cpu?: string
  memory?: string
  ip?: string
  containers?: number
}

interface TopoEdge {
  id: string
  source: string
  target: string
  label: string | null
}

interface TopologyResponse {
  nodes: TopoNode[]
  edges: TopoEdge[]
}

interface CloudAccount {
  id: string
  account_id: string | null
  name: string
  provider: string
  is_mock: boolean
}

interface ClusterItem {
  id: string
  name: string
  cloud: string
  cluster_type: string
  account_id: string | null
  region: string | null
  status: string
}

// ─── State ──────────────────────────────────────────────────────
const loading = ref(false)
const refreshing = ref(false)
const flowNodes = ref<any[]>([])
const flowEdges = ref<any[]>([])

// Account / Cluster selectors — global state so selection persists across navigation
const accounts = ref<CloudAccount[]>([])
const clusters = ref<ClusterItem[]>([])
const selectedAccountId = useState('topo-accountId', () => '__all__')
const selectedClusterId = useState('topo-clusterId', () => '__all__')
const lastRefreshAt = ref<Date | null>(null)

const nodeTypes = { topology: markRaw(TopologyNode) } as any
const { fitView } = useVueFlow({ id: 'service-topology' })

// ─── Computed ───────────────────────────────────────────────────
const filteredClusters = computed(() => {
  if (selectedAccountId.value === '__all__') return clusters.value
  return clusters.value.filter(c => c.account_id === selectedAccountId.value)
})

const awsAccounts = computed(() => accounts.value.filter(a => !a.is_mock))

const lastRefreshLabel = computed(() => {
  if (!lastRefreshAt.value) return ''
  const now = Date.now()
  const diff = now - lastRefreshAt.value.getTime()
  const secs = Math.floor(diff / 1000)
  if (secs < 60) return `${secs}s ago`
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `${mins}m ago`
  return `${Math.floor(mins / 60)}h${mins % 60}m ago`
})

// ─── Edge styles by kind pair ────────────────────────────────────
const edgeStyleMap: Record<string, Record<string, any>> = {
  'ingress-service':     { stroke: '#a855f7', strokeWidth: 1.5 },
  'service-deployment':  { stroke: '#3b82f6', strokeWidth: 1.5 },
  'service-rollout':     { stroke: '#f97316', strokeWidth: 2 },
  'deployment-pod':      { stroke: '#10b981', strokeWidth: 1 },
  'rollout-pod':         { stroke: '#f97316', strokeWidth: 1 },
  'pod-node':            { stroke: '#ec4899', strokeWidth: 1 },
  default:               { stroke: '#6b7280', strokeWidth: 1 },
}

function getEdgeStyle(sourceKind: string, targetKind: string) {
  return edgeStyleMap[`${sourceKind}-${targetKind}`] || edgeStyleMap.default
}

// ─── Auto-layout: 5-tier left-to-right ──────────────────────────
// Ingress (x=0) → Service (x=260) → Deployment/Rollout (x=520) → Pod (x=780) → Node (x=1080)
function layoutGraph(apiNodes: TopoNode[], apiEdges: TopoEdge[]) {
  const nodeMap = new Map(apiNodes.map(n => [n.id, n]))

  // Separate nodes into namespace-scoped and cluster-scoped
  const nsNodes = apiNodes.filter(n => n.kind !== 'node')
  const nodeNodes = apiNodes.filter(n => n.kind === 'node')

  // Group namespace-scoped by namespace+cluster
  interface NsGroup {
    namespace: string
    cluster: string
    ingresses: TopoNode[]
    services: TopoNode[]
    workloads: TopoNode[]
    pods: TopoNode[]
  }
  const nsGroups = new Map<string, NsGroup>()

  for (const n of nsNodes) {
    const key = `${n.cluster_id}/${n.namespace}`
    if (!nsGroups.has(key)) {
      nsGroups.set(key, { namespace: n.namespace, cluster: n.cluster, ingresses: [], services: [], workloads: [], pods: [] })
    }
    const g = nsGroups.get(key)!
    if (n.kind === 'ingress') g.ingresses.push(n)
    else if (n.kind === 'service') g.services.push(n)
    else if (n.kind === 'pod') g.pods.push(n)
    else g.workloads.push(n)
  }

  const nodes: any[] = []
  const edges: any[] = []
  const ROW_H = 72
  const COL_X = { ingress: 40, service: 280, workload: 520, pod: 760, node: 1060 }
  let groupY = 0

  for (const [key, group] of nsGroups) {
    const maxRows = Math.max(group.ingresses.length, group.services.length, group.workloads.length, group.pods.length, 1)
    const groupHeight = maxRows * ROW_H + 60

    // Zone background (non-interactive)
    nodes.push({
      id: `zone-${key}`,
      type: 'group',
      position: { x: 0, y: groupY },
      draggable: false,
      selectable: false,
      connectable: false,
      focusable: false,
      style: {
        width: '1000px',
        height: `${groupHeight}px`,
        background: 'rgba(255,255,255,0.015)',
        border: '1px dashed rgba(255,255,255,0.06)',
        borderRadius: '12px',
        pointerEvents: 'none',
      },
      data: { label: '' },
    })

    // Zone label
    const labelId = `label-${key}`
    nodes.push({
      id: labelId,
      type: 'group',
      position: { x: 8, y: groupY + 6 },
      draggable: false,
      selectable: false,
      connectable: false,
      focusable: false,
      style: {
        width: 'auto', height: 'auto',
        background: 'transparent', border: 'none', pointerEvents: 'none',
        fontSize: '9px', fontWeight: '600', color: 'rgba(255,255,255,0.25)',
        textTransform: 'uppercase', letterSpacing: '0.1em',
      },
      data: { label: `${group.namespace} @ ${group.cluster}` },
    })

    const baseY = 36

    // Place each tier
    const tiers: { col: number; items: TopoNode[] }[] = [
      { col: COL_X.ingress, items: group.ingresses },
      { col: COL_X.service, items: group.services },
      { col: COL_X.workload, items: group.workloads },
      { col: COL_X.pod, items: group.pods },
    ]

    for (const tier of tiers) {
      tier.items.forEach((n, i) => {
        nodes.push({
          id: n.id,
          type: 'topology',
          draggable: false,
          position: { x: tier.col, y: groupY + baseY + i * ROW_H },
          data: {
            label: n.label, subtitle: n.subtitle, kind: n.kind, status: n.status,
            replicas: n.replicas, namespace: n.namespace, cluster: n.cluster,
            cpu: n.cpu, memory: n.memory, ip: n.ip, containers: n.containers,
          },
        })
      })
    }

    groupY += groupHeight + 20
  }

  // ─── Place Nodes (cluster-scoped, separate column to the right) ───
  // Group nodes by cluster
  const nodesByCluster = new Map<string, TopoNode[]>()
  for (const n of nodeNodes) {
    const arr = nodesByCluster.get(n.cluster_id) || []
    arr.push(n)
    nodesByCluster.set(n.cluster_id, arr)
  }

  let nodeY = 0
  for (const [clusterId, clusterNodes] of nodesByCluster) {
    const clusterName = clusterNodes[0]?.cluster || clusterId

    // Node zone label
    nodes.push({
      id: `node-label-${clusterId}`,
      type: 'group',
      position: { x: COL_X.node, y: nodeY },
      draggable: false,
      selectable: false,
      connectable: false,
      focusable: false,
      style: {
        width: 'auto', height: 'auto',
        background: 'transparent', border: 'none', pointerEvents: 'none',
        fontSize: '9px', fontWeight: '600', color: 'rgba(236,72,153,0.4)',
        textTransform: 'uppercase', letterSpacing: '0.1em',
      },
      data: { label: `nodes @ ${clusterName}` },
    })

    nodeY += 24

    clusterNodes.forEach((n, i) => {
      nodes.push({
        id: n.id,
        type: 'topology',
        draggable: false,
        position: { x: COL_X.node, y: nodeY + i * ROW_H },
        data: {
          label: n.label, subtitle: n.subtitle, kind: n.kind, status: n.status,
          replicas: n.replicas, namespace: n.namespace, cluster: n.cluster,
          cpu: n.cpu, memory: n.memory,
        },
      })
    })

    nodeY += clusterNodes.length * ROW_H + 20
  }

  // Build edges with styles
  for (const e of apiEdges) {
    const srcNode = nodeMap.get(e.source)
    const tgtNode = nodeMap.get(e.target)
    if (!srcNode || !tgtNode) continue

    const style = getEdgeStyle(srcNode.kind, tgtNode.kind)
    edges.push({
      id: e.id,
      source: e.source,
      target: e.target,
      animated: srcNode.kind !== 'pod', // Pod→Node edges are static
      label: e.label || undefined,
      style,
    })
  }

  return { nodes, edges }
}

// ─── Data fetching ─────────────────────────────────────────────
async function fetchAccounts() {
  try {
    accounts.value = await api.get<CloudAccount[]>('/api/accounts')
  } catch { /* silent */ }
}

async function fetchClusters() {
  try {
    clusters.value = await api.get<ClusterItem[]>('/api/clusters')
  } catch { /* silent */ }
}

// ─── Topology API ──────────────────────────────────────────────
async function fetchTopology(forceRefresh = false) {
  loading.value = true
  try {
    const params = new URLSearchParams()
    if (forceRefresh) params.set('force_refresh', 'true')
    if (selectedClusterId.value && selectedClusterId.value !== '__all__') {
      params.set('cluster_id', selectedClusterId.value)
    }
    const qs = params.toString()
    const url = qs ? `/api/topology?${qs}` : '/api/topology'
    const data = await api.get<TopologyResponse>(url)
    const { nodes, edges } = layoutGraph(data.nodes, data.edges)
    flowNodes.value = nodes
    flowEdges.value = edges
    lastRefreshAt.value = new Date()
    nextTick(() => { setTimeout(() => fitView({ padding: 0.15 }), 200) })
  } catch {
    // silent — empty state shown
  } finally {
    loading.value = false
  }
}

async function refresh() {
  refreshing.value = true
  await fetchTopology(true)
  refreshing.value = false
}

// ─── Watchers ──────────────────────────────────────────────────
// When account changes, reset cluster selection and reload
watch(selectedAccountId, () => {
  selectedClusterId.value = '__all__'
  fetchTopology()
})

// When cluster changes, reload topology
watch(selectedClusterId, () => {
  fetchTopology()
})

// ─── Lifecycle ─────────────────────────────────────────────────
onMounted(async () => {
  await Promise.all([fetchAccounts(), fetchClusters()])
  fetchTopology()
})

// ─── Legend ──────────────────────────────────────────────────────
const kindLegend = [
  { color: 'bg-purple-400', label: 'Ingress' },
  { color: 'bg-blue-400', label: 'Service' },
  { color: 'bg-emerald-400', label: 'Deployment' },
  { color: 'bg-orange-400', label: 'Rollout' },
  { color: 'bg-cyan-400', label: 'Pod' },
  { color: 'bg-pink-400', label: 'Node' },
]

const statusLegend = [
  { color: 'bg-emerald-400', label: t('topology.healthy') },
  { color: 'bg-amber-400', label: t('topology.warning') },
  { color: 'bg-red-400 animate-pulse', label: t('topology.critical') },
]
</script>

<template>
  <div class="space-y-3">
    <!-- Header -->
    <div class="flex items-center justify-between flex-wrap gap-3">
      <div class="flex items-center gap-3">
        <div class="p-2 rounded-lg bg-blue-500/10 border border-blue-500/20">
          <Network class="h-5 w-5 text-blue-400" />
        </div>
        <div>
          <h1 class="text-base font-semibold text-foreground">{{ t('topology.title') }}</h1>
          <p class="text-[11px] text-muted-foreground">{{ t('topology.subtitle') }}</p>
        </div>
      </div>

      <div class="flex items-center gap-2">
        <!-- Account selector -->
        <Select v-model="selectedAccountId">
          <SelectTrigger class="h-8 w-[180px] text-[11px]">
            <SelectValue :placeholder="t('topology.selectAccount')" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="__all__">{{ t('topology.allAccounts') }}</SelectItem>
            <SelectItem v-for="acct in awsAccounts" :key="acct.id" :value="acct.account_id || acct.id">
              {{ acct.name }}
              <span v-if="acct.account_id" class="text-muted-foreground/50 ml-1">{{ acct.account_id }}</span>
            </SelectItem>
          </SelectContent>
        </Select>

        <!-- Cluster selector -->
        <Select v-model="selectedClusterId">
          <SelectTrigger class="h-8 w-[200px] text-[11px]">
            <SelectValue :placeholder="t('topology.selectCluster')" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="__all__">{{ t('topology.allClusters') }} ({{ filteredClusters.length }})</SelectItem>
            <SelectItem v-for="c in filteredClusters" :key="c.id" :value="c.id">
              {{ c.name }}
              <span v-if="c.region" class="text-muted-foreground/50 ml-1">{{ c.region }}</span>
            </SelectItem>
          </SelectContent>
        </Select>

        <!-- Separator -->
        <div class="h-5 w-px bg-border/40" />

        <!-- Legend (desktop) -->
        <div class="hidden xl:flex items-center gap-2">
          <div v-for="item in kindLegend" :key="item.label" class="flex items-center gap-1">
            <span class="h-2 w-2 rounded-full" :class="item.color" />
            <span class="text-[10px] text-muted-foreground">{{ item.label }}</span>
          </div>
          <div class="h-3 w-px bg-border/50" />
          <div v-for="item in statusLegend" :key="item.label" class="flex items-center gap-1">
            <span class="h-2 w-2 rounded-full shadow-[0_0_4px]" :class="item.color" />
            <span class="text-[10px] text-muted-foreground">{{ item.label }}</span>
          </div>
        </div>

        <!-- Cache indicator + Refresh -->
        <Tooltip v-if="lastRefreshLabel">
          <TooltipTrigger as-child>
            <span class="text-[10px] text-muted-foreground/50 flex items-center gap-1">
              <Clock class="h-3 w-3" />
              {{ lastRefreshLabel }}
            </span>
          </TooltipTrigger>
          <TooltipContent side="bottom" class="text-xs">
            {{ t('topology.lastRefresh') }}: {{ lastRefreshAt?.toLocaleTimeString() }}
          </TooltipContent>
        </Tooltip>

        <Tooltip>
          <TooltipTrigger as-child>
            <Button variant="ghost" size="sm" class="h-8 w-8 p-0" :disabled="refreshing || loading" @click="refresh">
              <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': refreshing }" />
            </Button>
          </TooltipTrigger>
          <TooltipContent side="bottom" class="text-xs">
            {{ t('topology.refresh') }} (force)
          </TooltipContent>
        </Tooltip>
      </div>
    </div>

    <!-- Loading -->
    <div v-if="loading && flowNodes.length === 0" class="flex flex-col items-center justify-center rounded-lg border border-border/60 bg-card/30" style="height: calc(100vh - 140px)">
      <Loader2 class="h-6 w-6 animate-spin text-blue-400 mb-3" />
      <p class="text-sm text-muted-foreground">{{ t('topology.loading') }}</p>
    </div>

    <!-- Empty -->
    <div v-else-if="flowNodes.length === 0" class="flex flex-col items-center justify-center rounded-lg border border-border/60 bg-card/30" style="height: calc(100vh - 140px)">
      <Network class="h-10 w-10 opacity-20 mb-3" />
      <p class="text-sm text-muted-foreground">{{ t('topology.empty') }}</p>
    </div>

    <!-- Topology Canvas -->
    <div v-else class="relative rounded-lg border border-border/60 bg-card/30 overflow-hidden" style="height: calc(100vh - 140px)">
      <!-- Loading overlay when refreshing -->
      <div v-if="refreshing || loading" class="absolute inset-0 z-20 flex items-center justify-center bg-black/30 backdrop-blur-[1px]">
        <div class="flex items-center gap-2 rounded-lg bg-card/80 border border-border/50 px-4 py-2">
          <Loader2 class="h-4 w-4 animate-spin text-blue-400" />
          <span class="text-xs text-muted-foreground">{{ t('topology.loading') }}</span>
        </div>
      </div>

      <!-- Flow direction labels -->
      <div class="absolute top-3 left-3 z-10 flex items-center gap-1.5 pointer-events-none">
        <Badge variant="outline" class="text-[9px] px-1.5 py-0 border-purple-500/30 text-purple-400">Ingress</Badge>
        <span class="text-zinc-600 text-[10px]">→</span>
        <Badge variant="outline" class="text-[9px] px-1.5 py-0 border-blue-500/30 text-blue-400">Service</Badge>
        <span class="text-zinc-600 text-[10px]">→</span>
        <Badge variant="outline" class="text-[9px] px-1.5 py-0 border-emerald-500/30 text-emerald-400">Workload</Badge>
        <span class="text-zinc-600 text-[10px]">→</span>
        <Badge variant="outline" class="text-[9px] px-1.5 py-0 border-cyan-500/30 text-cyan-400">Pod</Badge>
        <span class="text-zinc-600 text-[10px]">→</span>
        <Badge variant="outline" class="text-[9px] px-1.5 py-0 border-pink-500/30 text-pink-400">Node</Badge>
      </div>

      <VueFlow
        id="service-topology"
        :nodes="flowNodes"
        :edges="flowEdges"
        :node-types="nodeTypes"
        :default-viewport="{ x: 40, y: 20, zoom: 0.75 }"
        :min-zoom="0.15"
        :max-zoom="2.5"
        :nodes-draggable="false"
        :nodes-connectable="false"
        :snap-to-grid="true"
        :snap-grid="[10, 10]"
        fit-view-on-init
        class="topology-flow"
      >
        <MiniMap
          :node-color="(n: any) => {
            const kind = n.data?.kind
            if (kind === 'ingress') return '#a855f7'
            if (kind === 'service') return '#3b82f6'
            if (kind === 'rollout') return '#f97316'
            if (kind === 'deployment') return '#10b981'
            if (kind === 'pod') return '#06b6d4'
            if (kind === 'node') return '#ec4899'
            return '#6b7280'
          }"
          :mask-color="'rgba(0,0,0,0.7)'"
          class="!bg-black/40 !border-border/30 !rounded-lg"
        />
        <Controls class="!bg-black/40 !border-border/30 !rounded-lg" />
      </VueFlow>
    </div>
  </div>
</template>

<style>
/* Vue Flow dark theme overrides */
.topology-flow .vue-flow__background {
  background: transparent;
}
.topology-flow .vue-flow__edge-path {
  filter: drop-shadow(0 0 2px currentColor);
}
.topology-flow .vue-flow__edge-text {
  font-size: 9px;
  fill: rgba(255,255,255,0.4);
}
.topology-flow .vue-flow__edge-textbg {
  fill: rgba(17, 18, 23, 0.8);
  rx: 3;
}
.topology-flow .vue-flow__controls-button {
  background: rgba(17, 18, 23, 0.8) !important;
  border-color: rgba(255,255,255,0.1) !important;
  color: rgba(255,255,255,0.5) !important;
}
.topology-flow .vue-flow__controls-button:hover {
  background: rgba(30, 32, 40, 0.9) !important;
  color: rgba(255,255,255,0.8) !important;
}
.topology-flow .vue-flow__minimap {
  background: rgba(0,0,0,0.5) !important;
}
.topology-flow .vue-flow__edge.animated path {
  animation: edgeGlow 3s ease-in-out infinite;
}
@keyframes edgeGlow {
  0%, 100% { filter: drop-shadow(0 0 1px currentColor); opacity: 0.8; }
  50% { filter: drop-shadow(0 0 4px currentColor); opacity: 1; }
}
</style>
