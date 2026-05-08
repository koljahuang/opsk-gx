/**
 * SSE streaming proxy for POST /api/issues/:id/rca
 *
 * Returns the backend error as JSON (not Nuxt HTML error) so the client
 * can parse it and display a clean message / trigger token refresh.
 */
export default defineEventHandler(async (event) => {
  const id = getRouterParam(event, 'id')
  const config = useRuntimeConfig()
  const backendUrl = `${config.backendUrl}/api/issues/${id}/rca`
  const cookie = getHeader(event, 'cookie') || ''

  const response = await fetch(backendUrl, {
    method: 'POST',
    headers: { 'Cookie': cookie },
  })

  if (!response.ok) {
    const body = await response.text().catch(() => '{"error":"Backend error"}')
    setResponseStatus(event, response.status)
    setResponseHeader(event, 'content-type', 'application/json')
    return body
  }

  await proxySseResponse(event, response)
})
