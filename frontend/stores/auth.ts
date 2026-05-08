import { defineStore } from 'pinia'

interface User {
  id: string
  username: string
  role: 'super_admin' | 'tenant_admin' | 'operator' | 'member' | 'viewer'
  tenant_id: string | null
  email: string | null
  auth_method: string
  must_change_password: boolean
}

interface AuthProviders {
  local: boolean
  microsoft: boolean
  cognito: boolean
  is_cloud: boolean
  cognito_logout_url?: string
}

export const useAuthStore = defineStore('auth', {
  state: () => ({
    user: null as User | null,
    isAuthenticated: false,
    isLoading: true,
    providers: null as AuthProviders | null,
    isRefreshing: false,
    _refreshPromise: null as Promise<boolean> | null,
  }),

  getters: {
    isSuperAdmin: (state) => state.user?.role === 'super_admin',
    isTenantAdmin: (state) => state.user?.role === 'tenant_admin',
    isAdmin: (state) => state.user?.role === 'super_admin' || state.user?.role === 'tenant_admin',
    tenantId: (state) => state.user?.tenant_id,
  },

  actions: {
    async fetchProviders() {
      try {
        const config = useRuntimeConfig()
        const baseURL = config.public.apiBase || ''
        const headers: Record<string, string> = {}
        if (import.meta.server) {
          try {
            const requestHeaders = useRequestHeaders(['cookie'])
            if (requestHeaders.cookie) {
              headers['Cookie'] = requestHeaders.cookie
            }
          } catch {
            // useRequestHeaders may fail outside of Nuxt context — ignore
          }
        }
        const resp = await fetch(`${baseURL}/api/auth/providers`, { headers, credentials: 'include' })
        if (resp.ok) {
          this.providers = await resp.json()
        }
      } catch {
        // Fallback: show local login only (safe default for local dev / SSR failure)
        if (!this.providers) {
          this.providers = { local: true, microsoft: false, cognito: false, is_cloud: false }
        }
      }
    },

    async fetchMe() {
      this.isLoading = true
      try {
        const api = useApi()
        this.user = await api.get<User>('/api/auth/me')
        this.isAuthenticated = true
      } catch {
        this.user = null
        this.isAuthenticated = false
      } finally {
        this.isLoading = false
      }
    },

    async login(username: string, password: string) {
      const api = useApi()
      const response = await api.post<{ user: User; token: string }>('/api/auth/login', {
        username,
        password,
      })
      this.user = response.user
      this.isAuthenticated = true
    },

    /** Called after OAuth callback completes — tokens are already in cookies */
    setOAuthUser(user: User) {
      this.user = user
      this.isAuthenticated = true
    },

    /** Refresh access token using refresh token cookie.
     *  Concurrent callers share the same in-flight request. */
    async refreshAccessToken(): Promise<boolean> {
      // If a refresh is already in progress, wait for its result
      if (this._refreshPromise) return this._refreshPromise

      this._refreshPromise = this._doRefresh()
      try {
        return await this._refreshPromise
      } finally {
        this._refreshPromise = null
      }
    },

    async _doRefresh(): Promise<boolean> {
      try {
        const api = useApi()
        const response = await api.post<{ user: User; token: string }>('/api/auth/refresh')
        this.user = response.user
        this.isAuthenticated = true
        return true
      } catch {
        this.user = null
        this.isAuthenticated = false
        return false
      }
    },

    async logout() {
      const cognitoLogoutUrl = this.providers?.cognito_logout_url
      try {
        const api = useApi()
        await api.post('/api/auth/logout')
      } finally {
        this.user = null
        this.isAuthenticated = false
      }
      // Redirect to Cognito logout to clear hosted UI session
      if (cognitoLogoutUrl) {
        // Override logout_uri with current origin so user always returns
        // to the domain they're on (not a stale ALB/internal URL)
        const url = new URL(cognitoLogoutUrl)
        url.searchParams.set('logout_uri', window.location.origin + '/login')
        window.location.href = url.toString()
      }
    },

    async logoutAll() {
      const cognitoLogoutUrl = this.providers?.cognito_logout_url
      try {
        const api = useApi()
        await api.post('/api/auth/revoke-all')
        await api.post('/api/auth/logout')
      } finally {
        this.user = null
        this.isAuthenticated = false
      }
      if (cognitoLogoutUrl) {
        const url = new URL(cognitoLogoutUrl)
        url.searchParams.set('logout_uri', window.location.origin + '/login')
        window.location.href = url.toString()
      }
    },
  },
})
