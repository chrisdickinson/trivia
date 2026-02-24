import { useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { api } from '../api'

interface Props {
  onClose: () => void
  onCreated: () => void
}

export function CreateMemoryDialog({ onClose, onCreated }: Props) {
  const [mnemonic, setMnemonic] = useState('')
  const [content, setContent] = useState('')
  const [tagsStr, setTagsStr] = useState('')

  const mutation = useMutation({
    mutationFn: () => {
      const tags = tagsStr.split(',').map(t => t.trim()).filter(Boolean)
      return api.createMemory(mnemonic, content, tags)
    },
    onSuccess: onCreated,
  })

  return (
    <div className="fixed inset-0 bg-black/40 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-white rounded-lg p-6 w-[32rem]" onClick={e => e.stopPropagation()}>
        <h2 className="font-semibold mb-4">New Memory</h2>
        <div className="space-y-3">
          <input
            type="text"
            placeholder="Mnemonic"
            value={mnemonic}
            onChange={e => setMnemonic(e.target.value)}
            className="w-full border rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
          />
          <textarea
            placeholder="Content"
            value={content}
            onChange={e => setContent(e.target.value)}
            rows={5}
            className="w-full border rounded-lg px-3 py-2 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-500"
          />
          <input
            type="text"
            placeholder="Tags (comma-separated)"
            value={tagsStr}
            onChange={e => setTagsStr(e.target.value)}
            className="w-full border rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
          />
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
            disabled={!mnemonic || !content || mutation.isPending}
            className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50"
          >
            {mutation.isPending ? 'Creating...' : 'Create'}
          </button>
        </div>
      </div>
    </div>
  )
}
