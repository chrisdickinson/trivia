import { useEffect, useRef } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useNavigate } from 'react-router-dom'
import * as d3 from 'd3'
import { api } from '../api'

interface SimNode extends d3.SimulationNodeDatum {
  mnemonic: string
  content: string
  tags: string[]
  recall_count: number
}

interface SimLink extends d3.SimulationLinkDatum<SimNode> {
  link_type: string
}

const LINK_COLORS: Record<string, string> = {
  related: '#9ca3af',
  supersedes: '#ef4444',
  derived_from: '#3b82f6',
}

export function GraphView() {
  const svgRef = useRef<SVGSVGElement>(null)
  const navigate = useNavigate()

  const { data, isLoading } = useQuery({
    queryKey: ['graph'],
    queryFn: api.getGraph,
  })

  useEffect(() => {
    if (!data || !svgRef.current) return

    const svg = d3.select(svgRef.current)
    svg.selectAll('*').remove()

    const width = svgRef.current.clientWidth
    const height = svgRef.current.clientHeight

    const nodeMap = new Map<string, SimNode>()
    const nodes: SimNode[] = data.nodes.map(n => {
      const sn: SimNode = { ...n }
      nodeMap.set(n.mnemonic, sn)
      return sn
    })

    const links: SimLink[] = data.edges
      .filter(e => nodeMap.has(e.source) && nodeMap.has(e.target))
      .map(e => ({
        source: e.source,
        target: e.target,
        link_type: e.link_type,
      }))

    // Arrow markers
    const defs = svg.append('defs')
    for (const [type, color] of Object.entries(LINK_COLORS)) {
      defs.append('marker')
        .attr('id', `arrow-${type}`)
        .attr('viewBox', '0 -5 10 10')
        .attr('refX', 20)
        .attr('refY', 0)
        .attr('markerWidth', 6)
        .attr('markerHeight', 6)
        .attr('orient', 'auto')
        .append('path')
        .attr('d', 'M0,-5L10,0L0,5')
        .attr('fill', color)
    }

    const g = svg.append('g')

    const zoom = d3.zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.1, 4])
      .on('zoom', (event) => g.attr('transform', event.transform))
    svg.call(zoom)

    const simulation = d3.forceSimulation(nodes)
      .force('link', d3.forceLink<SimNode, SimLink>(links).id(d => d.mnemonic).distance(100))
      .force('charge', d3.forceManyBody().strength(-200))
      .force('center', d3.forceCenter(width / 2, height / 2))

    const link = g.append('g')
      .selectAll('line')
      .data(links)
      .join('line')
      .attr('stroke', d => LINK_COLORS[d.link_type] ?? '#9ca3af')
      .attr('stroke-width', 1.5)
      .attr('marker-end', d => `url(#arrow-${d.link_type})`)

    const node = g.append('g')
      .selectAll<SVGCircleElement, SimNode>('circle')
      .data(nodes)
      .join('circle')
      .attr('r', d => 5 + Math.min(d.recall_count, 20))
      .attr('fill', '#3b82f6')
      .attr('stroke', '#fff')
      .attr('stroke-width', 1.5)
      .style('cursor', 'pointer')
      .on('click', (_event, d) => {
        navigate(`/memory/${encodeURIComponent(d.mnemonic)}`)
      })
      .call(d3.drag<SVGCircleElement, SimNode>()
        .on('start', (event, d) => {
          if (!event.active) simulation.alphaTarget(0.3).restart()
          d.fx = d.x
          d.fy = d.y
        })
        .on('drag', (event, d) => {
          d.fx = event.x
          d.fy = event.y
        })
        .on('end', (event, d) => {
          if (!event.active) simulation.alphaTarget(0)
          d.fx = null
          d.fy = null
        })
      )

    const label = g.append('g')
      .selectAll('text')
      .data(nodes)
      .join('text')
      .text(d => d.mnemonic)
      .attr('font-size', 10)
      .attr('dx', d => 8 + Math.min(d.recall_count, 20))
      .attr('dy', 3)
      .attr('fill', '#374151')
      .style('pointer-events', 'none')

    simulation.on('tick', () => {
      link
        .attr('x1', d => (d.source as SimNode).x!)
        .attr('y1', d => (d.source as SimNode).y!)
        .attr('x2', d => (d.target as SimNode).x!)
        .attr('y2', d => (d.target as SimNode).y!)
      node.attr('cx', d => d.x!).attr('cy', d => d.y!)
      label.attr('x', d => d.x!).attr('y', d => d.y!)
    })

    return () => { simulation.stop() }
  }, [data, navigate])

  if (isLoading) return <p className="text-gray-500 text-sm">Loading graph...</p>

  return (
    <div className="border rounded-lg bg-white overflow-hidden" style={{ height: 'calc(100vh - 140px)' }}>
      <div className="flex gap-4 px-4 py-2 text-xs text-gray-500 border-b">
        <span className="flex items-center gap-1"><span className="inline-block w-3 h-0.5 bg-gray-400"></span> related</span>
        <span className="flex items-center gap-1"><span className="inline-block w-3 h-0.5 bg-red-500"></span> supersedes</span>
        <span className="flex items-center gap-1"><span className="inline-block w-3 h-0.5 bg-blue-500"></span> derived_from</span>
      </div>
      <svg ref={svgRef} className="w-full h-full" />
    </div>
  )
}
