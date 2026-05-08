/**
 * Admin middleware — blocks member-role users from admin-only pages.
 * Must be used AFTER 'auth' middleware (user must be loaded first).
 */
export default defineNuxtRouteMiddleware(() => {
  if (import.meta.server) return

  const authStore = useAuthStore()

  if (authStore.user && !authStore.isAdmin) {
    return navigateTo('/')
  }
})
