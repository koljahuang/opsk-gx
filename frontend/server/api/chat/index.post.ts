/**
 * SSE streaming proxy for POST /api/chat
 */
export default defineEventHandler(async (event) => {
  const body = await readBody(event)
  const config = useRuntimeConfig()
  const backendUrl = `${config.backendUrl}/api/chat`
  const cookie = getHeader(event, 'cookie') || ''

  const response = await fetch(backendUrl, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Cookie': cookie,
    },
    body: JSON.stringify(body),
  })

  if (!response.ok) {
    const body = await response.text().catch(() => '{"error":"Backend error"}')
    setResponseStatus(event, response.status)
    setResponseHeader(event, 'content-type', 'application/json')
    return body
  }

  await proxySseResponse(event, response)
})
