export interface MemorySummary {
  mnemonic: string
  content: string
  tags: string[]
  mnemonics?: string[]
  recall_count: number
}

export interface Memory {
  mnemonic: string
  content: string
  tags: string[]
  mnemonics?: string[]
  distance: number
  score: number
  updated_at: string
  recall_count: number
  last_recalled_at: string | null
  links: MemoryLink[]
}

export interface MemoryLink {
  source_mnemonic: string
  target_mnemonic: string
  link_type: string
  created_at: string
}

export interface GraphData {
  nodes: GraphNode[]
  edges: GraphEdge[]
}

export interface GraphNode {
  mnemonic: string
  content: string
  tags: string[]
  mnemonics?: string[]
  recall_count: number
}

export interface GraphEdge {
  source: string
  target: string
  link_type: string
}

export interface TagCount {
  tag: string
  count: number
}

const enc = (s: string) => encodeURIComponent(s)

declare global {
  interface Window { __TRIVIA_BASE__?: string }
}

/** Path prefix injected by the server (e.g. "/trivia"); empty for root mount. */
export const BASE_PATH: string = (() => {
  const raw = (typeof window !== 'undefined' ? window.__TRIVIA_BASE__ : '') ?? ''
  return raw.replace(/\/+$/, '')
})()

/** Prefix an absolute server path with the configured base path. */
export const url = (path: string): string => `${BASE_PATH}${path}`

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const text = await res.text()
    throw new Error(`${res.status}: ${text}`)
  }
  return res.json()
}

export interface AuthUser {
  username: string
  acl: string
}

export interface AuthProviders {
  providers: string[]
}

export const auth = {
  me: () =>
    fetch(url('/auth/me')).then(r => {
      if (r.status === 401) return null
      return json<AuthUser>(r)
    }).catch(() => null),

  providers: () =>
    fetch(url('/auth/providers')).then(r => json<AuthProviders>(r)).catch(() => ({ providers: [] })),

  logout: () =>
    fetch(url('/auth/logout'), { method: 'POST' }),
}

export const api = {
  listMemories: () =>
    fetch(url('/api/memories')).then(r => json<MemorySummary[]>(r)),

  getMemory: (mnemonic: string) =>
    fetch(url(`/api/memories/${enc(mnemonic)}`)).then(r => json<Memory>(r)),

  createMemory: (mnemonic: string, content: string, tags: string[]) =>
    fetch(url('/api/memories'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mnemonic, content, tags }),
    }).then(r => json<{ ok: boolean }>(r)),

  updateMemory: (mnemonic: string, content: string, tags: string[], newMnemonic?: string) =>
    fetch(url(`/api/memories/${enc(mnemonic)}`), {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content, tags, ...(newMnemonic && newMnemonic !== mnemonic ? { mnemonic: newMnemonic } : {}) }),
    }).then(r => json<{ ok: boolean; mnemonic?: string }>(r)),

  deleteMemory: (mnemonic: string) =>
    fetch(url(`/api/memories/${enc(mnemonic)}`), { method: 'DELETE' })
      .then(r => json<{ ok: boolean }>(r)),

  search: (q: string, limit = 10, tags?: string[]) => {
    const params = new URLSearchParams({ q, limit: String(limit) })
    if (tags && tags.length > 0) params.set('tags', tags.join(','))
    return fetch(url(`/api/search?${params}`)).then(r => json<Memory[]>(r))
  },

  listTags: () =>
    fetch(url('/api/tags')).then(r => json<TagCount[]>(r)),

  getGraph: () =>
    fetch(url('/api/graph')).then(r => json<GraphData>(r)),

  merge: (keep: string, discard: string) =>
    fetch(url('/api/memories/merge'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ keep, discard }),
    }).then(r => json<{ ok: boolean }>(r)),

  createLink: (source: string, target: string, link_type: string) =>
    fetch(url('/api/links'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ source, target, link_type }),
    }).then(r => json<{ ok: boolean }>(r)),

  removeLink: (source: string, target: string, link_type: string) =>
    fetch(url('/api/links'), {
      method: 'DELETE',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ source, target, link_type }),
    }).then(r => json<{ ok: boolean }>(r)),

  addMnemonic: (title: string, text: string) =>
    fetch(url(`/api/memories/${enc(title)}/mnemonics`), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text }),
    }).then(r => json<{ ok: boolean }>(r)),

  removeMnemonic: (title: string, text: string) =>
    fetch(url(`/api/memories/${enc(title)}/mnemonics`), {
      method: 'DELETE',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text }),
    }).then(r => json<{ ok: boolean }>(r)),
}
