import { useState } from 'react'
import { useQuery, useMutation } from '@tanstack/react-query'
import { api } from '../api'

interface Props {
  mnemonicA: string
  mnemonicB: string
  onClose: () => void
  onMerged: () => void
}

export function MergeDialog({ mnemonicA, mnemonicB, onClose, onMerged }: Props) {
  const [keep, setKeep] = useState(mnemonicA)
  const discard = keep === mnemonicA ? mnemonicB : mnemonicA

  const memA = useQuery({ queryKey: ['memory', mnemonicA], queryFn: () => api.getMemory(mnemonicA) })
  const memB = useQuery({ queryKey: ['memory', mnemonicB], queryFn: () => api.getMemory(mnemonicB) })

  const mutation = useMutation({
    mutationFn: () => api.merge(keep, discard),
    onSuccess: onMerged,
  })

  const loading = memA.isLoading || memB.isLoading

  return (
    <div className="fixed inset-0 bg-black/40 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-white rounded-lg p-6 w-[48rem] max-h-[80vh] overflow-y-auto" onClick={e => e.stopPropagation()}>
        <h2 className="font-semibold mb-4">Merge Memories</h2>
        {loading ? (
          <p className="text-gray-500 text-sm">Loading...</p>
        ) : (
          <div className="grid grid-cols-2 gap-4 mb-4">
            {[{ mnemonic: mnemonicA, data: memA.data }, { mnemonic: mnemonicB, data: memB.data }].map(({ mnemonic, data }) => (
              <div
                key={mnemonic}
                onClick={() => setKeep(mnemonic)}
                className={`border rounded-lg p-4 cursor-pointer ${
                  keep === mnemonic ? 'border-green-500 bg-green-50' : 'border-gray-200 hover:border-gray-400'
                }`}
              >
                <div className="flex items-center justify-between mb-2">
                  <span className="font-mono text-sm font-bold">{mnemonic}</span>
                  {keep === mnemonic ? (
                    <span className="text-xs bg-green-200 text-green-800 px-1.5 py-0.5 rounded">KEEP</span>
                  ) : (
                    <span className="text-xs bg-red-200 text-red-800 px-1.5 py-0.5 rounded">DISCARD</span>
                  )}
                </div>
                <pre className="text-xs whitespace-pre-wrap text-gray-600 max-h-40 overflow-y-auto">{data?.content}</pre>
                {data && data.tags.length > 0 && (
                  <div className="flex gap-1 mt-2 flex-wrap">
                    {data.tags.map(t => (
                      <span key={t} className="bg-gray-200 text-gray-700 px-1.5 py-0.5 rounded text-xs">{t}</span>
                    ))}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
        {mutation.isError && (
          <p className="text-red-600 text-xs mb-2">{(mutation.error as Error).message}</p>
        )}
        <div className="flex gap-2 justify-end">
          <button onClick={onClose} className="px-3 py-1.5 text-sm border rounded hover:bg-gray-50">
            Cancel
          </button>
          <button
            onClick={() => mutation.mutate()}
            disabled={mutation.isPending}
            className="px-3 py-1.5 text-sm bg-orange-600 text-white rounded hover:bg-orange-700 disabled:opacity-50"
          >
            {mutation.isPending ? 'Merging...' : `Merge (keep ${keep})`}
          </button>
        </div>
      </div>
    </div>
  )
}
