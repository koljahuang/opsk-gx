/**
 * Proxy GET /api/issues/:id/rca/status to backend.
 */
export default defineEventHandler(async (event) => {
  const id = getRouterParam(event, 'id')
  const config = useRuntimeConfig()
  const backendUrl = `${config.backendUrl}/api/issues/${id}/rca/status`

  const cookie = getHeader(event, 'cookie') || ''

  const response = await fetch(backendUrl, {
    headers: { 'Cookie': cookie },
  })

  if (!response.ok) {
    const err = await response.text().catch(() => 'Backend error')
    throw createError({
      statusCode: response.status,
      statusMessage: err,
    })
  }

  return await response.json()
})
