import { useState } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../api'
import { LinkDialog } from '../components/LinkDialog'

export function MemoryDetail() {
  const { mnemonic } = useParams<{ mnemonic: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const decodedMnemonic = decodeURIComponent(mnemonic ?? '')

  const { data: memory, isLoading } = useQuery({
    queryKey: ['memory', decodedMnemonic],
    queryFn: () => api.getMemory(decodedMnemonic),
    enabled: !!decodedMnemonic,
  })

  const [editing, setEditing] = useState(false)
  const [content, setContent] = useState('')
  const [tagsStr, setTagsStr] = useState('')
  const [showDelete, setShowDelete] = useState(false)
  const [showLink, setShowLink] = useState(false)

  const startEdit = () => {
    if (!memory) return
    setContent(memory.content)
    setTagsStr(memory.tags.join(', '))
    setEditing(true)
  }

  const updateMutation = useMutation({
    mutationFn: () => {
      const tags = tagsStr.split(',').map(t => t.trim()).filter(Boolean)
      return api.updateMemory(decodedMnemonic, content, tags)
    },
    onSuccess: () => {
      setEditing(false)
      queryClient.invalidateQueries({ queryKey: ['memory', decodedMnemonic] })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: () => api.deleteMemory(decodedMnemonic),
    onSuccess: () => navigate('/'),
  })

  const unlinkMutation = useMutation({
    mutationFn: (args: { source: string; target: string; link_type: string }) =>
      api.removeLink(args.source, args.target, args.link_type),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['memory', decodedMnemonic] }),
  })

  if (isLoading) return <p className="text-gray-500 text-sm">Loading...</p>
  if (!memory) return <p className="text-gray-500 text-sm">Memory not found.</p>

  return (
    <div className="max-w-3xl">
      <button onClick={() => navigate('/')} className="text-sm text-gray-500 hover:text-gray-700 mb-4">
        &larr; Back
      </button>

      <div className="bg-white border rounded-lg p-6">
        <div className="flex items-start justify-between mb-4">
          <h1 className="font-mono text-xl font-bold text-blue-700">{decodedMnemonic}</h1>
          <div className="flex gap-2">
            {!editing && (
              <button onClick={startEdit} className="px-3 py-1 text-sm border rounded hover:bg-gray-50">
                Edit
              </button>
            )}
            <button onClick={() => setShowLink(true)} className="px-3 py-1 text-sm border rounded hover:bg-gray-50">
              Link
            </button>
            <button onClick={() => setShowDelete(true)} className="px-3 py-1 text-sm border border-red-300 text-red-600 rounded hover:bg-red-50">
              Delete
            </button>
          </div>
        </div>

        {editing ? (
          <div className="space-y-3">
            <textarea
              value={content}
              onChange={e => setContent(e.target.value)}
              rows={8}
              className="w-full border rounded-lg px-3 py-2 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-500"
            />
            <input
              type="text"
              value={tagsStr}
              onChange={e => setTagsStr(e.target.value)}
              placeholder="Tags (comma-separated)"
              className="w-full border rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
            />
            <div className="flex gap-2">
              <button
                onClick={() => updateMutation.mutate()}
                disabled={updateMutation.isPending}
                className="px-4 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-50"
              >
                {updateMutation.isPending ? 'Saving...' : 'Save'}
              </button>
              <button onClick={() => setEditing(false)} className="px-4 py-2 text-sm border rounded-lg hover:bg-gray-50">
                Cancel
              </button>
            </div>
          </div>
        ) : (
          <div className="space-y-3">
            <pre className="whitespace-pre-wrap text-sm leading-relaxed">{memory.content}</pre>
            {memory.tags.length > 0 && (
              <div className="flex gap-1.5 flex-wrap">
                {memory.tags.map(t => (
                  <span key={t} className="bg-gray-200 text-gray-700 px-2 py-0.5 rounded text-xs">{t}</span>
                ))}
              </div>
            )}
            <div className="text-xs text-gray-400 space-x-4">
              <span>Updated: {memory.updated_at}</span>
              <span>Recalls: {memory.recall_count}</span>
              {memory.last_recalled_at && <span>Last recalled: {memory.last_recalled_at}</span>}
            </div>
          </div>
        )}

        {memory.links.length > 0 && (
          <div className="mt-6 border-t pt-4">
            <h2 className="text-sm font-semibold text-gray-600 mb-2">Links</h2>
            <div className="space-y-1">
              {memory.links.map((l, i) => {
                const other = l.source_mnemonic === decodedMnemonic ? l.target_mnemonic : l.source_mnemonic
                const direction = l.source_mnemonic === decodedMnemonic ? '\u2192' : '\u2190'
                return (
                  <div key={i} className="flex items-center gap-2 text-sm">
                    <span className="text-gray-400">{direction}</span>
                    <button
                      onClick={() => navigate(`/memory/${encodeURIComponent(other)}`)}
                      className="font-mono text-blue-600 hover:underline"
                    >
                      {other}
                    </button>
                    <span className={`text-xs px-1.5 py-0.5 rounded ${
                      l.link_type === 'supersedes' ? 'bg-red-100 text-red-700' :
                      l.link_type === 'derived_from' ? 'bg-blue-100 text-blue-700' :
                      'bg-gray-100 text-gray-600'
                    }`}>
                      {l.link_type}
                    </span>
                    <button
                      onClick={() => unlinkMutation.mutate({ source: l.source_mnemonic, target: l.target_mnemonic, link_type: l.link_type })}
                      className="text-xs text-red-400 hover:text-red-600 ml-auto"
                    >
                      unlink
                    </button>
                  </div>
                )
              })}
            </div>
          </div>
        )}
      </div>

      {showDelete && (
        <div className="fixed inset-0 bg-black/40 flex items-center justify-center z-50" onClick={() => setShowDelete(false)}>
          <div className="bg-white rounded-lg p-6 w-96" onClick={e => e.stopPropagation()}>
            <h2 className="font-semibold mb-2">Delete memory?</h2>
            <p className="text-sm text-gray-600 mb-4">
              This will permanently delete <span className="font-mono">{decodedMnemonic}</span> and all its links.
            </p>
            <div className="flex gap-2 justify-end">
              <button onClick={() => setShowDelete(false)} className="px-3 py-1.5 text-sm border rounded hover:bg-gray-50">
                Cancel
              </button>
              <button
                onClick={() => deleteMutation.mutate()}
                disabled={deleteMutation.isPending}
                className="px-3 py-1.5 text-sm bg-red-600 text-white rounded hover:bg-red-700 disabled:opacity-50"
              >
                Delete
              </button>
            </div>
          </div>
        </div>
      )}

      {showLink && (
        <LinkDialog
          currentMnemonic={decodedMnemonic}
          onClose={() => setShowLink(false)}
          onLinked={() => {
            setShowLink(false)
            queryClient.invalidateQueries({ queryKey: ['memory', decodedMnemonic] })
          }}
        />
      )}
    </div>
  )
}
