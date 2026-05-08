/**
 * Auth middleware — redirects unauthenticated users to /login
 * Client-only: SSR fetch() cannot forward browser HttpOnly cookies.
 */
export default defineNuxtRouteMiddleware(async (to) => {
  if (import.meta.server) return

  // Skip auth check for login page and OAuth callback pages
  if (to.path === '/login' || to.path.startsWith('/auth/')) return

  const authStore = useAuthStore()

  // Fetch user info and providers if not loaded yet
  if (!authStore.user && authStore.isLoading) {
    await Promise.all([
      authStore.fetchMe(),
      authStore.providers ? Promise.resolve() : authStore.fetchProviders(),
    ])
  }

  // If not authenticated, try refreshing the access token before redirecting
  if (!authStore.isAuthenticated) {
    const refreshed = await authStore.refreshAccessToken()
    if (refreshed) {
      await authStore.fetchMe()
    }
  }

  // Redirect to login if still not authenticated
  if (!authStore.isAuthenticated) {
    return navigateTo('/login')
  }

  // Force password change if required (except on settings page itself)
  if (authStore.user?.must_change_password && to.path !== '/settings') {
    return navigateTo('/settings')
  }
})
