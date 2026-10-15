<script setup lang="ts">
import { ref, onMounted } from 'vue'
import {
  LayoutDashboard,
  Network,
  Sparkles,
  Rss,
  Activity,
  RefreshCw,
  Search,
  ShieldCheck,
  Send,
  Zap,
  Globe
} from 'lucide-vue-next'

type Tab = 'dashboard' | 'nodes' | 'rules' | 'subs'

const currentTab = ref<Tab>('dashboard')
const systemInfo = ref<any>(null)
const nodesList = ref<any[]>([])
const nodeStats = ref({ total: 0, healthy: 0, cooldown: 0 })
const rulesList = ref<any[]>([])
const searchQuery = ref('')
const statusFilter = ref('all')
const probingNodeId = ref<number | null>(null)
const toastMessage = ref<string | null>(null)

// Rules sandbox state
const testUrl = ref('https://api.github.com/repos/rust-lang/rust')
const testHeaders = ref('x-github-user: octocat\nAuthorization: Bearer test_token_123')
const resolveResult = ref<any>(null)
const isResolving = ref(false)

// Subscription sync state
const subUrl = ref('')
const subText = ref('')
const isSyncing = ref(false)

function showToast(msg: string) {
  toastMessage.value = msg
  setTimeout(() => {
    toastMessage.value = null
  }, 3500)
}

async function fetchSystemInfo() {
  try {
    const res = await fetch('/api/v1/system/info')
    if (res.ok) {
      systemInfo.value = await res.json()
    }
  } catch (err) {
    console.error('System info fetch error:', err)
  }
}

async function fetchNodes() {
  try {
    const query = new URLSearchParams()
    if (statusFilter.value !== 'all') query.set('status', statusFilter.value)
    if (searchQuery.value) query.set('search', searchQuery.value)

    const res = await fetch(`/api/v1/nodes?${query.toString()}`)
    if (res.ok) {
      const data = await res.json()
      nodesList.value = data.items
      nodeStats.value = {
        total: data.total,
        healthy: data.healthy_count,
        cooldown: data.cooldown_count
      }
    }
  } catch (err) {
    console.error('Nodes fetch error:', err)
  }
}

async function fetchRules() {
  try {
    const res = await fetch('/api/v1/rules')
    if (res.ok) {
      rulesList.value = await res.json()
    }
  } catch (err) {
    console.error('Rules fetch error:', err)
  }
}

async function probeSingleNode(id: number) {
  probingNodeId.value = id
  try {
    const res = await fetch(`/api/v1/nodes/${id}/actions/probe`, { method: 'POST' })
    const data = await res.json()
    if (data.success) {
      showToast(`Probe Succeeded! Node #${id} latency: ${data.latency_ms}ms`)
    } else {
      showToast(`Node #${id} probe failed: ${data.error}`)
    }
    await fetchNodes()
  } catch (err: any) {
    showToast(`Probe request failed: ${err.message}`)
  } finally {
    probingNodeId.value = null
  }
}

async function runRuleResolve() {
  isResolving.value = true
  try {
    const headersMap: Record<string, string> = {}
    for (const line of testHeaders.value.split('\n')) {
      const idx = line.indexOf(':')
      if (idx > 0) {
        headersMap[line.slice(0, idx).trim()] = line.slice(idx + 1).trim()
      }
    }

    const res = await fetch('/api/v1/rules:resolve', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ url: testUrl.value, headers: headersMap })
    })
    if (res.ok) {
      resolveResult.value = await res.json()
    } else {
      showToast('Resolution failed, please verify URL format')
    }
  } catch (err: any) {
    showToast(`Error: ${err.message}`)
  } finally {
    isResolving.value = false
  }
}

async function syncSubscription() {
  isSyncing.value = true
  try {
    const body: any = {}
    if (subUrl.value.trim()) body.url = subUrl.value.trim()
    if (subText.value.trim()) body.content = subText.value.trim()

    const res = await fetch('/api/v1/subscriptions/sync', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    })
    if (res.ok) {
      const data = await res.json()
      showToast(`Imported ${data.imported_count} nodes! Total: ${data.total_nodes}`)
      await fetchNodes()
      await fetchSystemInfo()
    } else {
      const text = await res.text()
      showToast(`Import failed: ${text}`)
    }
  } catch (err: any) {
    showToast(`Sync failed: ${err.message}`)
  } finally {
    isSyncing.value = false
  }
}

onMounted(() => {
  fetchSystemInfo()
  fetchNodes()
  fetchRules()
  setInterval(() => {
    fetchSystemInfo()
    if (currentTab.value === 'nodes') fetchNodes()
  }, 10000)
})
</script>

<template>
  <div class="flex min-h-screen bg-slate-950 font-sans text-slate-100 antialiased">
    <!-- Toast Message -->
    <div
      v-if="toastMessage"
      class="fixed bottom-6 right-6 z-50 rounded-lg border border-sky-500 bg-slate-900/95 px-5 py-3 text-sm text-sky-200 shadow-xl backdrop-blur transition-all"
    >
      {{ toastMessage }}
    </div>

    <!-- Sidebar -->
    <aside class="flex w-64 flex-col border-r border-slate-800 bg-slate-900/50 backdrop-blur">
      <div class="flex items-center gap-3 border-b border-slate-800 px-6 py-5">
        <Activity class="h-6 w-6 text-sky-400" />
        <div>
          <h2 class="text-base font-bold tracking-tight text-white">Proxy Gateway</h2>
          <span class="text-xs text-slate-400">Next-Gen Intelligent Pool</span>
        </div>
      </div>

      <nav class="flex flex-1 flex-col gap-1.5 p-4">
        <button
          class="flex items-center gap-3 rounded-md px-3.5 py-2.5 text-sm font-medium transition-all"
          :class="currentTab === 'dashboard' ? 'bg-sky-500/15 text-sky-400' : 'text-slate-400 hover:bg-slate-800 hover:text-white'"
          @click="currentTab = 'dashboard'"
        >
          <LayoutDashboard class="h-4 w-4" />
          <span>Dashboard</span>
        </button>
        <button
          class="flex items-center gap-3 rounded-md px-3.5 py-2.5 text-sm font-medium transition-all"
          :class="currentTab === 'nodes' ? 'bg-sky-500/15 text-sky-400' : 'text-slate-400 hover:bg-slate-800 hover:text-white'"
          @click="currentTab = 'nodes'; fetchNodes()"
        >
          <Network class="h-4 w-4" />
          <span>Node Pool</span>
        </button>
        <button
          class="flex items-center gap-3 rounded-md px-3.5 py-2.5 text-sm font-medium transition-all"
          :class="currentTab === 'rules' ? 'bg-sky-500/15 text-sky-400' : 'text-slate-400 hover:bg-slate-800 hover:text-white'"
          @click="currentTab = 'rules'; fetchRules()"
        >
          <Sparkles class="h-4 w-4" />
          <span>Rules & Sandbox</span>
        </button>
        <button
          class="flex items-center gap-3 rounded-md px-3.5 py-2.5 text-sm font-medium transition-all"
          :class="currentTab === 'subs' ? 'bg-sky-500/15 text-sky-400' : 'text-slate-400 hover:bg-slate-800 hover:text-white'"
          @click="currentTab = 'subs'"
        >
          <Rss class="h-4 w-4" />
          <span>Subscriptions</span>
        </button>
      </nav>

      <div class="border-t border-slate-800 p-5 text-xs text-slate-400">
        <div class="flex items-center gap-2 text-emerald-400">
          <span class="h-2 w-2 rounded-full bg-emerald-400 shadow-[0_0_8px_rgba(52,211,153,0.8)]"></span>
          <span>Zero D-State (Pure In-Memory)</span>
        </div>
        <p class="mt-2 text-slate-500">v{{ systemInfo?.version || '0.1.0' }} (Bun + Vue 3)</p>
      </div>
    </aside>

    <!-- Main Content Area -->
    <main class="flex-1 overflow-y-auto p-8">
      <!-- 1. DASHBOARD -->
      <section v-if="currentTab === 'dashboard'" class="space-y-6">
        <div>
          <h1 class="text-2xl font-bold tracking-tight text-white">Gateway Dashboard</h1>
          <p class="mt-1 text-sm text-slate-400">Single-Port Protocol Sniffing, Sticky Session Leases & Lock-Free Proxy Pool</p>
        </div>

        <div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <div class="rounded-xl border border-slate-800 bg-slate-900/40 p-5 backdrop-blur">
            <span class="text-xs font-medium text-slate-400">Total Nodes</span>
            <div class="mt-2 text-3xl font-extrabold text-sky-400">{{ systemInfo?.total_nodes ?? 0 }}</div>
            <span class="mt-1 block text-xs text-slate-500">ArcSwap Atomic Hot-Reload</span>
          </div>
          <div class="rounded-xl border border-slate-800 bg-slate-900/40 p-5 backdrop-blur">
            <span class="text-xs font-medium text-slate-400">Healthy Nodes</span>
            <div class="mt-2 text-3xl font-extrabold text-emerald-400">{{ nodeStats.healthy }}</div>
            <span class="mt-1 block text-xs text-slate-500">P2C + TD-EWMA Scheduled</span>
          </div>
          <div class="rounded-xl border border-slate-800 bg-slate-900/40 p-5 backdrop-blur">
            <span class="text-xs font-medium text-slate-400">Cooling Down</span>
            <div class="mt-2 text-3xl font-extrabold text-rose-400">{{ nodeStats.cooldown }}</div>
            <span class="mt-1 block text-xs text-slate-500">Exponential Backoff Recovery</span>
          </div>
          <div class="rounded-xl border border-slate-800 bg-slate-900/40 p-5 backdrop-blur">
            <span class="text-xs font-medium text-slate-400">Proxy Listen Port</span>
            <div class="mt-2 text-3xl font-extrabold text-slate-100">{{ systemInfo?.proxy_port ?? 1099 }}</div>
            <span class="mt-1 block text-xs text-slate-500">SOCKS5 / HTTP Hybrid Sniffing</span>
          </div>
        </div>

        <div class="rounded-xl border border-slate-800 bg-slate-900/40 p-6">
          <h3 class="text-base font-semibold text-white">Architecture Invariants & Principles</h3>
          <div class="mt-4 grid grid-cols-1 gap-4 md:grid-cols-3">
            <div class="flex gap-3.5 rounded-lg border border-slate-800/80 bg-slate-950/50 p-4">
              <ShieldCheck class="mt-0.5 h-5 w-5 shrink-0 text-emerald-400" />
              <div>
                <strong class="text-sm text-slate-200">Pure In-Memory (Zero Disk I/O)</strong>
                <p class="mt-1 text-xs leading-relaxed text-slate-400">Eliminated all SQLite/db disk flushes on data paths. Runtime state stays 100% in RAM, eliminating D-state sleep.</p>
              </div>
            </div>
            <div class="flex gap-3.5 rounded-lg border border-slate-800/80 bg-slate-950/50 p-4">
              <Globe class="mt-0.5 h-5 w-5 shrink-0 text-sky-400" />
              <div>
                <strong class="text-sm text-slate-200">O(1) Same Egress IP Failover</strong>
                <p class="mt-1 text-xs leading-relaxed text-slate-400">Maintains reverse Egress IP index for instantaneous failover to backup nodes with identical public IP.</p>
              </div>
            </div>
            <div class="flex gap-3.5 rounded-lg border border-slate-800/80 bg-slate-950/50 p-4">
              <Zap class="mt-0.5 h-5 w-5 shrink-0 text-amber-400" />
              <div>
                <strong class="text-sm text-slate-200">Zero-Intrusion URL Trie Matcher</strong>
                <p class="mt-1 text-xs leading-relaxed text-slate-400">Extracts tenant accounts via segment-based longest prefix matching with lock-free ArcSwap hot swaps.</p>
              </div>
            </div>
          </div>
        </div>
      </section>

      <!-- 2. NODES MANAGEMENT -->
      <section v-if="currentTab === 'nodes'" class="space-y-4">
        <div>
          <h1 class="text-2xl font-bold tracking-tight text-white">Proxy Node Pool</h1>
          <p class="mt-1 text-sm text-slate-400">Live node metrics, consecutive failure counters & manual probing</p>
        </div>

        <div class="flex items-center gap-3">
          <div class="relative flex-1">
            <Search class="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-slate-400" />
            <input
              v-model="searchQuery"
              placeholder="Filter by tag or dial address..."
              class="w-full rounded-lg border border-slate-800 bg-slate-900/60 py-2 pl-9 pr-4 text-sm text-slate-200 outline-none focus:border-sky-500"
              @input="fetchNodes"
            />
          </div>
          <select
            v-model="statusFilter"
            class="rounded-lg border border-slate-800 bg-slate-900/60 px-3.5 py-2 text-sm text-slate-200 outline-none focus:border-sky-500"
            @change="fetchNodes"
          >
            <option value="all">All Status</option>
            <option value="healthy">Healthy Only</option>
            <option value="cooldown">Cooldown Only</option>
          </select>
          <button
            class="inline-flex items-center gap-2 rounded-lg bg-sky-500 px-4 py-2 text-sm font-medium text-slate-950 transition hover:bg-sky-400"
            @click="fetchNodes"
          >
            <RefreshCw class="h-4 w-4" />
            <span>Refresh</span>
          </button>
        </div>

        <div class="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/40">
          <table class="w-full text-left text-sm">
            <thead class="border-b border-slate-800 bg-slate-900/80 text-xs font-semibold uppercase text-slate-400">
              <tr>
                <th class="px-5 py-3.5">ID</th>
                <th class="px-5 py-3.5">Tag</th>
                <th class="px-5 py-3.5">Status</th>
                <th class="px-5 py-3.5">Dial Address</th>
                <th class="px-5 py-3.5">Requests</th>
                <th class="px-5 py-3.5">Failures</th>
                <th class="px-5 py-3.5">Active Conns</th>
                <th class="px-5 py-3.5">TD-EWMA RTT</th>
                <th class="px-5 py-3.5">Actions</th>
              </tr>
            </thead>
            <tbody class="divide-y divide-slate-800/60 text-slate-300">
              <tr v-for="node in nodesList" :key="node.id" class="transition hover:bg-slate-800/40">
                <td class="px-5 py-3.5 font-mono text-xs text-slate-400">#{{ node.id }}</td>
                <td class="px-5 py-3.5 font-medium text-white">{{ node.tag }}</td>
                <td class="px-5 py-3.5">
                  <span
                    class="inline-flex items-center rounded-full px-2 py-0.5 text-xs font-semibold"
                    :class="{
                      'bg-emerald-500/15 text-emerald-400 border border-emerald-500/30': node.status === 'healthy',
                      'bg-rose-500/15 text-rose-400 border border-rose-500/30': node.status === 'cooldown',
                      'bg-amber-500/15 text-amber-400 border border-amber-500/30': node.status === 'half_open',
                    }"
                  >
                    {{ node.status }}
                  </span>
                </td>
                <td class="px-5 py-3.5 font-mono text-xs text-slate-400">{{ node.dial_addrs.join(', ') || 'Direct' }}</td>
                <td class="px-5 py-3.5">{{ node.total_requests }}</td>
                <td class="px-5 py-3.5">{{ node.consecutive_failures }}</td>
                <td class="px-5 py-3.5">{{ node.active_connections }}</td>
                <td class="px-5 py-3.5 font-mono text-xs text-sky-400">{{ node.latency_ewma_ms }}ms</td>
                <td class="px-5 py-3.5">
                  <button
                    class="inline-flex items-center gap-1.5 rounded border border-slate-700 bg-slate-800/60 px-2.5 py-1 text-xs text-slate-200 transition hover:bg-slate-700 disabled:opacity-50"
                    :disabled="probingNodeId === node.id"
                    @click="probeSingleNode(node.id)"
                  >
                    <RefreshCw v-if="probingNodeId === node.id" class="h-3.5 w-3.5 animate-spin" />
                    <span>{{ probingNodeId === node.id ? 'Probing...' : 'Probe' }}</span>
                  </button>
                </td>
              </tr>
              <tr v-if="nodesList.length === 0">
                <td colspan="9" class="px-5 py-8 text-center text-slate-500">No nodes matching filter</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <!-- 3. RULES & SANDBOX -->
      <section v-if="currentTab === 'rules'" class="space-y-6">
        <div>
          <h1 class="text-2xl font-bold tracking-tight text-white">Reverse Proxy Rules & Sandbox</h1>
          <p class="mt-1 text-sm text-slate-400">Longest prefix Trie matching, dynamic header account extraction & interactive sandbox testing</p>
        </div>

        <div class="grid grid-cols-1 gap-6 lg:grid-cols-2">
          <!-- Sandbox -->
          <div class="rounded-xl border border-slate-800 bg-slate-900/40 p-6">
            <h3 class="text-base font-semibold text-white">Rule Tester & Sandbox</h3>
            <p class="mt-1 text-xs text-slate-400">Input target URL and mock headers to verify real-time account extraction</p>

            <div class="mt-4 space-y-4">
              <div>
                <label class="block text-xs font-medium text-slate-300">Target Test URL</label>
                <input
                  v-model="testUrl"
                  placeholder="https://api.domain.com/v1/..."
                  class="mt-1.5 w-full rounded-lg border border-slate-800 bg-slate-950 px-3.5 py-2 text-sm text-slate-200 outline-none focus:border-sky-500"
                />
              </div>

              <div>
                <label class="block text-xs font-medium text-slate-300">Mock Request Headers (Key: Value per line)</label>
                <textarea
                  v-model="testHeaders"
                  rows="4"
                  placeholder="x-user: alice&#10;authorization: Bearer xxx"
                  class="mt-1.5 w-full rounded-lg border border-slate-800 bg-slate-950 px-3.5 py-2 text-sm font-mono text-slate-200 outline-none focus:border-sky-500"
                ></textarea>
              </div>

              <button
                class="flex w-full items-center justify-center gap-2 rounded-lg bg-sky-500 py-2.5 text-sm font-medium text-slate-950 transition hover:bg-sky-400 disabled:opacity-50"
                :disabled="isResolving"
                @click="runRuleResolve"
              >
                <Send class="h-4 w-4" />
                <span>{{ isResolving ? 'Resolving...' : 'Test Resolution' }}</span>
              </button>

              <div v-if="resolveResult" class="rounded-lg border border-slate-800 bg-slate-950 p-4 text-xs space-y-1.5">
                <p class="text-slate-400"><strong>Matched Rule Prefix:</strong> <span class="font-mono text-slate-200">{{ resolveResult.matched_prefix || 'None' }}</span></p>
                <p class="text-slate-400"><strong>Extracted Account:</strong> <span class="font-bold text-emerald-400">{{ resolveResult.extracted_account || '(empty / not found)' }}</span></p>
                <p class="text-slate-400"><strong>Inspected Headers:</strong> {{ resolveResult.headers_to_inspect.join(', ') }}</p>
              </div>
            </div>
          </div>

          <!-- Active Rules -->
          <div class="rounded-xl border border-slate-800 bg-slate-900/40 p-6">
            <h3 class="text-base font-semibold text-white">Active Rule Prefixes</h3>
            <div class="mt-4 space-y-3">
              <div
                v-for="rule in rulesList"
                :key="rule.url_prefix"
                class="rounded-lg border border-slate-800/80 bg-slate-950 p-4"
              >
                <div class="flex items-center justify-between">
                  <span class="font-mono text-sm font-semibold text-sky-400">{{ rule.url_prefix }}</span>
                  <span class="rounded-full bg-emerald-500/15 px-2 py-0.5 text-xs font-semibold text-emerald-400 border border-emerald-500/30">Active</span>
                </div>
                <p class="mt-2 text-xs text-slate-400">Header Inspection Order: {{ rule.headers.join(' → ') }}</p>
              </div>
            </div>
          </div>
        </div>
      </section>

      <!-- 4. SUBSCRIPTION SYNC -->
      <section v-if="currentTab === 'subs'" class="space-y-6">
        <div>
          <h1 class="text-2xl font-bold tracking-tight text-white">Subscription Management</h1>
          <p class="mt-1 text-sm text-slate-400">Synchronize and hot-reload proxy nodes from remote or Base64 feeds</p>
        </div>

        <div class="max-w-2xl rounded-xl border border-slate-800 bg-slate-900/40 p-6">
          <h3 class="text-base font-semibold text-white">Import External Subscription</h3>
          <p class="mt-1 text-xs text-slate-400">Fetch via remote subscription URL or paste raw/Base64 proxy lines directly</p>

          <div class="mt-4 space-y-4">
            <div>
              <label class="block text-xs font-medium text-slate-300">Remote Subscription URL (HTTP/HTTPS)</label>
              <input
                v-model="subUrl"
                placeholder="https://provider.example.com/sub/token"
                class="mt-1.5 w-full rounded-lg border border-slate-800 bg-slate-950 px-3.5 py-2 text-sm text-slate-200 outline-none focus:border-sky-500"
              />
            </div>

            <div>
              <label class="block text-xs font-medium text-slate-300">Or Paste Raw / Base64 Content</label>
              <textarea
                v-model="subText"
                rows="6"
                placeholder="socks5://user:pass@1.2.3.4:1080&#10;http://5.6.7.8:8080"
                class="mt-1.5 w-full rounded-lg border border-slate-800 bg-slate-950 px-3.5 py-2 font-mono text-xs text-slate-200 outline-none focus:border-sky-500"
              ></textarea>
            </div>

            <button
              class="inline-flex items-center gap-2 rounded-lg bg-sky-500 px-5 py-2.5 text-sm font-medium text-slate-950 transition hover:bg-sky-400 disabled:opacity-50"
              :disabled="isSyncing"
              @click="syncSubscription"
            >
              <RefreshCw class="h-4 w-4" :class="{ 'animate-spin': isSyncing }" />
              <span>{{ isSyncing ? 'Syncing...' : 'Sync & Reload Pool' }}</span>
            </button>
          </div>
        </div>
      </section>
    </main>
  </div>
</template>
