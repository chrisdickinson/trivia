import { Routes, Route, NavLink } from 'react-router-dom'
import { MemoryList } from './pages/MemoryList'
import { MemoryDetail } from './pages/MemoryDetail'
import { GraphView } from './pages/GraphView'

export default function App() {
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
