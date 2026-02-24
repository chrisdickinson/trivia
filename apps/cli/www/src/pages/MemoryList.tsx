import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from 'react-router-dom'
import { api, type MemorySummary } from '../api'
import { CreateMemoryDialog } from '../components/CreateMemoryDialog'
import { MergeDialog } from '../components/MergeDialog'

export function MemoryList() {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [search, setSearch] = useState('')
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [showCreate, setShowCreate] = useState(false)
  const [showMerge, setShowMerge] = useState(false)

  const { data: memories = [], isLoading } = useQuery({
    queryKey: ['memories'],
    queryFn: api.listMemories,
  })

  const searchQuery = useQuery({
    queryKey: ['search', search],
    queryFn: () => api.search(search, 20),
    enabled: search.length > 2,
  })

  const items: MemorySummary[] = search.length > 2
    ? (searchQuery.data ?? []).map(m => ({
        mnemonic: m.mnemonic,
        content: m.content,
        tags: m.tags,
        recall_count: m.recall_count,
      }))
    : memories

  const toggleSelect = (mnemonic: string) => {
    setSelected(prev => {
      const next = new Set(prev)
      if (next.has(mnemonic)) next.delete(mnemonic)
      else next.add(mnemonic)
      return next
    })
  }

  const mergeSelection = [...selected]

  return (
    <div>
      <div className="flex items-center gap-3 mb-4">
        <input
          type="text"
          placeholder="Search memories..."
          value={search}
          onChange={e => setSearch(e.target.value)}
          className="flex-1 border rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
        />
        <button
          onClick={() => setShowCreate(true)}
          className="px-4 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700"
        >
          New Memory
        </button>
        {mergeSelection.length === 2 && (
          <button
            onClick={() => setShowMerge(true)}
            className="px-4 py-2 bg-orange-600 text-white text-sm rounded-lg hover:bg-orange-700"
          >
            Merge Selected
          </button>
        )}
      </div>

      {isLoading ? (
        <p className="text-gray-500 text-sm">Loading...</p>
      ) : items.length === 0 ? (
        <p className="text-gray-500 text-sm">No memories found.</p>
      ) : (
        <table className="w-full text-sm border-collapse">
          <thead>
            <tr className="border-b text-left text-gray-500">
              <th className="py-2 w-8"></th>
              <th className="py-2 px-2">Mnemonic</th>
              <th className="py-2 px-2">Content</th>
              <th className="py-2 px-2">Tags</th>
              <th className="py-2 px-2 text-right">Recalls</th>
            </tr>
          </thead>
          <tbody>
            {items.map(m => (
              <tr
                key={m.mnemonic}
                className="border-b hover:bg-gray-100 cursor-pointer"
                onClick={() => navigate(`/memory/${encodeURIComponent(m.mnemonic)}`)}
              >
                <td className="py-2 px-1" onClick={e => { e.stopPropagation(); toggleSelect(m.mnemonic) }}>
                  <input
                    type="checkbox"
                    checked={selected.has(m.mnemonic)}
                    readOnly
                    className="rounded"
                  />
                </td>
                <td className="py-2 px-2 font-mono text-blue-700 whitespace-nowrap">{m.mnemonic}</td>
                <td className="py-2 px-2 max-w-md truncate">{m.content}</td>
                <td className="py-2 px-2">
                  <div className="flex gap-1 flex-wrap">
                    {m.tags.map(t => (
                      <span key={t} className="bg-gray-200 text-gray-700 px-1.5 py-0.5 rounded text-xs">{t}</span>
                    ))}
                  </div>
                </td>
                <td className="py-2 px-2 text-right tabular-nums">{m.recall_count}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {showCreate && (
        <CreateMemoryDialog
          onClose={() => setShowCreate(false)}
          onCreated={() => {
            setShowCreate(false)
            queryClient.invalidateQueries({ queryKey: ['memories'] })
          }}
        />
      )}

      {showMerge && mergeSelection.length === 2 && (
        <MergeDialog
          mnemonicA={mergeSelection[0]}
          mnemonicB={mergeSelection[1]}
          onClose={() => setShowMerge(false)}
          onMerged={() => {
            setShowMerge(false)
            setSelected(new Set())
            queryClient.invalidateQueries({ queryKey: ['memories'] })
          }}
        />
      )}
    </div>
  )
}
