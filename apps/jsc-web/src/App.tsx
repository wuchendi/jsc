import { compile, run } from '@cdlab/jsc'
import { useCallback, useEffect, useRef, useState } from 'react'
import { Editor } from '@/components/Editor'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { EXAMPLES } from '@/lib/examples'

const REPO_URL = 'https://github.com/cdLab996/jsc'

interface RunState {
  result: string
  stdout: string
  wasm: Uint8Array
  compileMs: number
  runMs: number
}

export function App() {
  const [source, setSource] = useState(EXAMPLES[0].source)
  const [active, setActive] = useState(EXAMPLES[0].label)
  const [running, setRunning] = useState(false)
  const [state, setState] = useState<RunState | null>(null)
  const [error, setError] = useState<string | null>(null)
  const sourceRef = useRef(source)
  sourceRef.current = source

  const onRun = useCallback(async () => {
    setRunning(true)
    setError(null)
    try {
      const src = sourceRef.current
      const c0 = performance.now()
      const wasm = await compile(src)
      const c1 = performance.now()
      const out = await run(src)
      const r1 = performance.now()
      setState({
        result: out.result,
        stdout: out.stdout,
        wasm,
        compileMs: c1 - c0,
        runMs: r1 - c1,
      })
    } catch (err) {
      setState(null)
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setRunning(false)
    }
  }, [])

  // Run the opening example once so the panels are never empty on arrival.
  useEffect(() => {
    void onRun()
  }, [onRun])

  const pickExample = (label: string) => {
    const ex = EXAMPLES.find((e) => e.label === label)
    if (!ex) return
    setActive(ex.label)
    setSource(ex.source)
  }

  return (
    <div className="flex min-h-svh flex-col bg-background text-foreground lg:h-svh lg:min-h-0 lg:overflow-hidden">
      <Header onRun={onRun} running={running} />

      <div className="mx-auto flex w-full max-w-6xl flex-wrap items-center gap-3 px-5 pt-5">
        <span className="font-mono text-xs text-muted-foreground">
          examples
        </span>
        <ToggleGroup
          variant="outline"
          size="sm"
          value={[active]}
          onValueChange={(value: string[]) => {
            const next = value[value.length - 1]
            if (next) pickExample(next)
          }}
          className="flex-wrap"
        >
          {EXAMPLES.map((ex) => (
            <ToggleGroupItem
              key={ex.label}
              value={ex.label}
              title={ex.note}
              className="font-mono text-xs data-[pressed]:text-wasm"
            >
              {ex.label}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </div>

      <main className="mx-auto grid w-full max-w-6xl flex-1 grid-cols-1 gap-4 px-5 py-5 lg:min-h-0 lg:grid-cols-2 lg:overflow-hidden">
        <Panel label="source" fill>
          <Editor value={source} onChange={setSource} onRun={onRun} />
        </Panel>

        <div className="flex flex-col gap-4 lg:min-h-0 lg:overflow-hidden">
          {error !== null ? (
            <Panel label="diagnostic">
              <pre className="overflow-auto font-mono text-[0.8125rem] leading-relaxed whitespace-pre text-destructive">
                {error}
              </pre>
            </Panel>
          ) : (
            <>
              <Panel label="result">
                <pre className="overflow-auto font-mono text-sm whitespace-pre-wrap text-foreground">
                  {state?.result ?? '—'}
                </pre>
              </Panel>
              <Panel label="console">
                <pre className="overflow-auto font-mono text-[0.8125rem] leading-relaxed whitespace-pre-wrap text-muted-foreground">
                  {state && state.stdout !== '' ? state.stdout : 'no output'}
                </pre>
              </Panel>
              <WasmPanel wasm={state?.wasm ?? null} />
            </>
          )}
        </div>
      </main>

      <StatusBar state={state} error={error} running={running} />
    </div>
  )
}

function Header({ onRun, running }: { onRun: () => void; running: boolean }) {
  return (
    <header className="border-b border-border">
      <div className="mx-auto flex w-full max-w-6xl items-center justify-between gap-4 px-5 py-4">
        <div className="flex items-baseline gap-3">
          <span className="font-mono text-xl font-bold tracking-tight">
            jsc<span className="text-wasm">▸</span>
          </span>
          <span className="hidden font-mono text-xs text-muted-foreground sm:inline">
            JavaScript <span className="text-wasm">→</span> WebAssembly, in your
            browser
          </span>
        </div>
        <div className="flex items-center gap-4">
          <a
            href={REPO_URL}
            target="_blank"
            rel="noreferrer"
            className="font-mono text-xs text-muted-foreground transition-colors hover:text-foreground"
          >
            source ↗
          </a>
          <Button onClick={onRun} disabled={running} className="font-mono">
            {running ? 'running…' : 'Run ⌘↵'}
          </Button>
        </div>
      </div>
    </header>
  )
}

function Panel({
  label,
  fill = false,
  children,
}: {
  label: string
  fill?: boolean
  children: React.ReactNode
}) {
  return (
    <Card className={fill ? 'lg:min-h-0 lg:flex-1' : undefined}>
      <CardHeader>
        <CardTitle>{label}</CardTitle>
      </CardHeader>
      <CardContent
        className={fill ? 'lg:min-h-0 lg:flex-1 lg:overflow-hidden' : undefined}
      >
        {children}
      </CardContent>
    </Card>
  )
}

function WasmPanel({ wasm }: { wasm: Uint8Array | null }) {
  return (
    <Panel label="emitted wasm" fill>
      {wasm ? (
        <div className="flex h-full min-h-[16rem] flex-col">
          <div className="mb-2 flex items-center gap-2 font-mono text-xs text-muted-foreground">
            <span className="text-wasm">\0asm</span>
            <Badge variant="outline" className="font-mono">
              {wasm.length.toLocaleString()} bytes
            </Badge>
          </div>
          <pre className="min-h-0 flex-1 overflow-auto font-mono text-[0.75rem] leading-relaxed">
            {hexDump(wasm)}
          </pre>
        </div>
      ) : (
        <span className="font-mono text-xs text-muted-foreground">—</span>
      )}
    </Panel>
  )
}

function StatusBar({
  state,
  error,
  running,
}: {
  state: RunState | null
  error: string | null
  running: boolean
}) {
  let content: React.ReactNode
  if (running) {
    content = <span className="text-muted-foreground">compiling…</span>
  } else if (error !== null) {
    content = (
      <span className="text-destructive">compile error — see diagnostic</span>
    )
  } else if (state) {
    content = (
      <>
        <Stat label="parsed" value="swc" />
        <Divider />
        <Stat
          label="wasm"
          value={`${state.wasm.length.toLocaleString()} B`}
          accent="wasm"
        />
        <Divider />
        <Stat label="compiled" value={`${state.compileMs.toFixed(1)} ms`} />
        <Divider />
        <Stat
          label="ran"
          value={`${state.runMs.toFixed(1)} ms`}
          accent="signal"
        />
      </>
    )
  } else {
    content = <span className="text-muted-foreground">ready</span>
  }
  return (
    <footer className="border-t border-border">
      <div className="mx-auto flex w-full max-w-6xl items-center gap-3 px-5 py-2 font-mono text-xs">
        {content}
      </div>
    </footer>
  )
}

function Stat({
  label,
  value,
  accent,
}: {
  label: string
  value: string
  accent?: 'wasm' | 'signal'
}) {
  const color =
    accent === 'wasm'
      ? 'text-wasm'
      : accent === 'signal'
        ? 'text-signal'
        : 'text-foreground'
  return (
    <span className="text-muted-foreground">
      {label} <span className={color}>{value}</span>
    </span>
  )
}

function Divider() {
  return <Separator orientation="vertical" className="!h-3.5" />
}

/** Render the first slice of the module as an offset + hex grid. */
function hexDump(bytes: Uint8Array, limit = 512): string {
  const slice = bytes.subarray(0, limit)
  const rows: string[] = []
  for (let i = 0; i < slice.length; i += 16) {
    const offset = i.toString(16).padStart(6, '0')
    const hex = Array.from(slice.subarray(i, i + 16))
      .map((b) => b.toString(16).padStart(2, '0'))
      .join(' ')
    rows.push(`${offset}  ${hex}`)
  }
  if (bytes.length > limit) {
    rows.push(`…      +${(bytes.length - limit).toLocaleString()} more bytes`)
  }
  return rows.join('\n')
}
