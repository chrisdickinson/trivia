import { Routes, Route, NavLink } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { MemoryList } from './pages/MemoryList'
import { MemoryDetail } from './pages/MemoryDetail'
import { GraphView } from './pages/GraphView'
import { auth } from './api'

function LoginPage() {
  const { data: providers } = useQuery({
    queryKey: ['auth-providers'],
    queryFn: () => auth.providers(),
  })

  if (!providers || providers.providers.length === 0) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-gray-50">
        <div className="text-center">
          <h1 className="text-2xl font-bold mb-2">Trivia</h1>
          <p className="text-gray-500">No login providers configured.</p>
        </div>
      </div>
    )
  }

  return (
    <div className="min-h-screen flex items-center justify-center bg-gray-50">
      <div className="bg-white rounded-lg shadow-sm border p-8 w-80">
        <h1 className="text-2xl font-bold mb-6 text-center">Trivia</h1>
        <div className="space-y-3">
          {providers.providers.map(name => (
            <a
              key={name}
              href={`/auth/login/${name}`}
              className="block w-full text-center px-4 py-2 border rounded-md hover:bg-gray-50 font-medium capitalize"
            >
              Sign in with {name}
            </a>
          ))}
        </div>
      </div>
    </div>
  )
}

export default function App() {
  const queryClient = useQueryClient()
  const { data: user, isLoading } = useQuery({
    queryKey: ['auth-me'],
    queryFn: () => auth.me(),
    retry: false,
  })

  // Check if auth is required by looking at provider availability
  const { data: providers } = useQuery({
    queryKey: ['auth-providers'],
    queryFn: () => auth.providers(),
    retry: false,
  })

  const authRequired = providers && providers.providers.length > 0

  if (isLoading) {
    return null
  }

  // If auth providers exist but user isn't logged in, show login
  if (authRequired && !user) {
    return <LoginPage />
  }

  const handleLogout = async () => {
    await auth.logout()
    queryClient.invalidateQueries({ queryKey: ['auth-me'] })
  }

  return (
    <div className="min-h-screen bg-gray-50 text-gray-900">
      <nav className="border-b bg-white px-6 py-3 flex gap-6 items-center">
        <span className="font-bold text-lg">Trivia</span>
        <NavLink to="/" end className={({ isActive }) => isActive ? 'text-blue-600 font-medium' : 'text-gray-500 hover:text-gray-800'}>
          Memories
        </NavLink>
        <NavLink to="/graph" className={({ isActive }) => isActive ? 'text-blue-600 font-medium' : 'text-gray-500 hover:text-gray-800'}>
          Graph
        </NavLink>
        {user && (
          <div className="ml-auto flex items-center gap-3">
            <span className="text-sm text-gray-500">{user.username}</span>
            <button
              onClick={handleLogout}
              className="text-sm text-gray-400 hover:text-gray-600"
            >
              Sign out
            </button>
          </div>
        )}
      </nav>
      <main className="max-w-6xl mx-auto p-6">
        <Routes>
          <Route path="/" element={<MemoryList />} />
          <Route path="/memory/:mnemonic" element={<MemoryDetail />} />
          <Route path="/graph" element={<GraphView />} />
        </Routes>
      </main>
    </div>
  )
}
