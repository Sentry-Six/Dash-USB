/** Prefer the cheap endpoint, but keep a newer UI usable against older devices. */
export function createLivenessProbe() {
  let endpoint = "/api/health"
  return async (signal?: AbortSignal): Promise<Response> => {
    let response = await fetch(endpoint, { signal, cache: "no-store" })
    const isHtml = (res: Response) => res.headers.get("content-type")?.includes("text/html")
    // Some older servers return the SPA for unknown API routes instead of 404.
    // An old auth middleware can reject the unknown route before its 404 handler.
    if (endpoint === "/api/health" && ([401, 404].includes(response.status) || (response.ok && isHtml(response)))) {
      endpoint = "/api/status"
      response = await fetch(endpoint, { signal, cache: "no-store" })
    }
    if (response.ok && isHtml(response)) throw new Error("Health check returned a web page")
    return response
  }
}
