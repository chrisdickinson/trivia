import { useState } from 'react'
import { useQuery, useMutation } from '@tanstack/react-query'
import { api } from '../api'

interface Props {
  currentMnemonic: string
  onClose: () => void
  onLinked: () => void
}

export function LinkDialog({ currentMnemonic, onClose, onLinked }: Props) {
  const [target, setTarget] = useState('')
  const [linkType, setLinkType] = useState('related')

  const { data: memories = [] } = useQuery({
    queryKey: ['memories'],
    queryFn: api.listMemories,
  })

  const others = memories.filter(m => m.mnemonic !== currentMnemonic)

  const mutation = useMutation({
    mutationFn: () => api.createLink(currentMnemonic, target, linkType),
    onSuccess: onLinked,
  })

  return (
    <div className="fixed inset-0 bg-black/40 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-white rounded-lg p-6 w-96" onClick={e => e.stopPropagation()}>
        <h2 className="font-semibold mb-4">Create Link</h2>
        <div className="space-y-3">
          <div>
            <label className="text-xs text-gray-500 block mb-1">From</label>
            <div className="font-mono text-sm bg-gray-50 px-3 py-2 rounded border">{currentMnemonic}</div>
          </div>
          <div>
            <label className="text-xs text-gray-500 block mb-1">To</label>
            <select
              value={target}
              onChange={e => setTarget(e.target.value)}
              className="w-full border rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
            >
              <option value="">Select a memory...</option>
              {others.map(m => (
                <option key={m.mnemonic} value={m.mnemonic}>{m.mnemonic}</option>
              ))}
            </select>
          </div>
          <div>
            <label className="text-xs text-gray-500 block mb-1">Type</label>
            <select
              value={linkType}
              onChange={e => setLinkType(e.target.value)}
              className="w-full border rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
            >
              <option value="related">related</option>
              <option value="supersedes">supersedes</option>
              <option value="derived_from">derived_from</option>
            </select>
          </div>
        </div>
        {mutation.isError && (
          <p className="text-red-600 text-xs mt-2">{(mutation.error as Error).message}</p>
        )}
        <div className="flex gap-2 justify-end mt-4">
          <button onClick={onClose} className="px-3 py-1.5 text-sm border rounded hover:bg-gray-50">
            Cancel
          </button>
          <button
            onClick={() => mutation.mutate()}
            disabled={!target || mutation.isPending}
            className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50"
          >
            {mutation.isPending ? 'Linking...' : 'Create Link'}
          </button>
        </div>
      </div>
    </div>
  )
}
