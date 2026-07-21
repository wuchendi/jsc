import { javascript } from '@codemirror/lang-javascript'
import { HighlightStyle, syntaxHighlighting } from '@codemirror/language'
import { EditorState } from '@codemirror/state'
import { EditorView, keymap, placeholder } from '@codemirror/view'
import { tags as t } from '@lezer/highlight'
import { basicSetup } from 'codemirror'
import { useEffect, useRef } from 'react'

// Editor chrome tuned to the app palette. Colours are read from CSS custom
// properties so the editor tracks the theme.
const chrome = EditorView.theme(
  {
    '&': { color: 'var(--color-foreground)', backgroundColor: 'transparent' },
    '.cm-content': { caretColor: 'var(--jsc-wasm)', padding: '14px 0' },
    '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--jsc-wasm)' },
    '&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection':
      {
        backgroundColor:
          'color-mix(in oklch, var(--jsc-wasm) 26%, transparent)',
      },
    '.cm-activeLine': {
      backgroundColor:
        'color-mix(in oklch, var(--color-foreground) 4%, transparent)',
    },
    '.cm-gutters': {
      color:
        'color-mix(in oklch, var(--color-muted-foreground) 60%, transparent)',
    },
    '.cm-activeLineGutter': {
      backgroundColor: 'transparent',
      color: 'var(--jsc-wasm)',
    },
    '.cm-lineNumbers .cm-gutterElement': { padding: '0 12px 0 8px' },
  },
  { dark: true },
)

// Keywords ride the wasm accent, literals ride the signal accent — the same two
// colours the rest of the app uses to mean "compile" and "run".
const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.modifier], color: 'var(--jsc-wasm)', fontWeight: '600' },
  { tag: [t.controlKeyword, t.operatorKeyword], color: 'var(--jsc-wasm)' },
  { tag: [t.number, t.bool, t.null], color: 'var(--jsc-signal)' },
  { tag: [t.string, t.special(t.string)], color: 'var(--jsc-signal)' },
  {
    tag: [t.function(t.variableName), t.function(t.propertyName)],
    color: 'var(--color-foreground)',
  },
  {
    tag: [t.propertyName],
    color: 'color-mix(in oklch, var(--color-foreground) 82%, transparent)',
  },
  {
    tag: [t.variableName, t.definition(t.variableName)],
    color: 'var(--color-foreground)',
  },
  {
    tag: [t.comment],
    color: 'var(--color-muted-foreground)',
    fontStyle: 'italic',
  },
  {
    tag: [t.operator, t.punctuation, t.bracket],
    color: 'var(--color-muted-foreground)',
  },
  { tag: [t.className, t.typeName], color: 'var(--jsc-wasm)' },
])

interface EditorProps {
  value: string
  onChange: (value: string) => void
  onRun: () => void
}

export function Editor({ value, onChange, onRun }: EditorProps) {
  const host = useRef<HTMLDivElement>(null)
  const view = useRef<EditorView | null>(null)
  // Keep the latest callbacks reachable from the (stable) CodeMirror extensions.
  const onChangeRef = useRef(onChange)
  const onRunRef = useRef(onRun)
  onChangeRef.current = onChange
  onRunRef.current = onRun

  // Build the editor once.
  useEffect(() => {
    if (!host.current) return
    const state = EditorState.create({
      doc: value,
      extensions: [
        basicSetup,
        javascript(),
        chrome,
        syntaxHighlighting(highlight),
        placeholder('Type JavaScript, then press Run (⌘/Ctrl + Enter)'),
        keymap.of([
          {
            key: 'Mod-Enter',
            run: () => {
              onRunRef.current()
              return true
            },
          },
        ]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) onChangeRef.current(u.state.doc.toString())
        }),
      ],
    })
    const v = new EditorView({ state, parent: host.current })
    view.current = v
    return () => {
      v.destroy()
      view.current = null
    }
    // Intentionally build once; external value changes are synced below.
  }, [])

  // Sync programmatic value changes (example picker) into the editor.
  useEffect(() => {
    const v = view.current
    if (!v || v.state.doc.toString() === value) return
    v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: value } })
  }, [value])

  return <div ref={host} className="h-full min-h-[24rem] overflow-hidden" />
}
