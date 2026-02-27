export interface MemorySummary {
  mnemonic: string
  content: string
  tags: string[]
  recall_count: number
}

export interface Memory {
  mnemonic: string
  content: string
  tags: string[]
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

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const text = await res.text()
    throw new Error(`${res.status}: ${text}`)
  }
  return res.json()
}

export const api = {
  listMemories: () =>
    fetch('/api/memories').then(r => json<MemorySummary[]>(r)),

  getMemory: (mnemonic: string) =>
    fetch(`/api/memories/${enc(mnemonic)}`).then(r => json<Memory>(r)),

  createMemory: (mnemonic: string, content: string, tags: string[]) =>
    fetch('/api/memories', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mnemonic, content, tags }),
    }).then(r => json<{ ok: boolean }>(r)),

  updateMemory: (mnemonic: string, content: string, tags: string[], newMnemonic?: string) =>
    fetch(`/api/memories/${enc(mnemonic)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ content, tags, ...(newMnemonic && newMnemonic !== mnemonic ? { mnemonic: newMnemonic } : {}) }),
    }).then(r => json<{ ok: boolean; mnemonic?: string }>(r)),

  deleteMemory: (mnemonic: string) =>
    fetch(`/api/memories/${enc(mnemonic)}`, { method: 'DELETE' })
      .then(r => json<{ ok: boolean }>(r)),

  search: (q: string, limit = 10, tags?: string[]) => {
    const params = new URLSearchParams({ q, limit: String(limit) })
    if (tags && tags.length > 0) params.set('tags', tags.join(','))
    return fetch(`/api/search?${params}`).then(r => json<Memory[]>(r))
  },

  listTags: () =>
    fetch('/api/tags').then(r => json<TagCount[]>(r)),

  getGraph: () =>
    fetch('/api/graph').then(r => json<GraphData>(r)),

  merge: (keep: string, discard: string) =>
    fetch('/api/memories/merge', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ keep, discard }),
    }).then(r => json<{ ok: boolean }>(r)),

  createLink: (source: string, target: string, link_type: string) =>
    fetch('/api/links', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ source, target, link_type }),
    }).then(r => json<{ ok: boolean }>(r)),

  removeLink: (source: string, target: string, link_type: string) =>
    fetch('/api/links', {
      method: 'DELETE',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ source, target, link_type }),
    }).then(r => json<{ ok: boolean }>(r)),
}
