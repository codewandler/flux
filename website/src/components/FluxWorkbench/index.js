import React, {useCallback, useEffect, useMemo, useRef, useState} from 'react';
import ExecutionEnvironment from '@docusaurus/ExecutionEnvironment';
import styles from './styles.module.css';

const FIXTURE_FILES = {
  'summarize-readme': {'README.md': '# flux\n\nA deterministic agent platform where the LLM is not the runtime.\n'},
  'cached-page': {'cache/page.html': '<main><h1>Cached Flux page</h1><p>Loaded from scratch.</p></main>\n'},
  'wait-for-artifact': {'target/release/flux': 'scratch fixture\n'},
  'rust-files': {
    'src/main.rs': 'fn main() { println!("flux"); }\n',
    'src/lib.rs': 'pub fn flux() -> bool { true }\n',
    'tests/smoke.rs': '#[test] fn smoke() { assert!(true); }\n',
  },
  'first-app-a': {
    'docs/product.md': '# Northstar product\n\nOffline edits synchronize automatically after the device reconnects.\n',
    'docs/policies.md': '# Support\n\nSupport hours are Monday–Friday, 09:00–17:00 Central European Time.\n',
  },
  'first-app-b': {
    'docs/product.md': '# Northstar product\n\nOffline edits synchronize automatically after the device reconnects.\n',
    'docs/policies.md': '# Support\n\nSupport hours are Monday–Friday, 09:00–17:00 Central European Time.\n',
  },
};

const FIXTURE_INPUTS = {
  'cached-page': {url: 'https://example.com'},
  'rust-files': {dirs: ['src', 'tests']},
};

let authPromise;
let nextWorkbenchId = 0;

async function bootstrapRuntime() {
  if (!ExecutionEnvironment.canUseDOM) return {execution: false, lsp: false};
  if (!authPromise) {
    authPromise = (async () => {
      const match = window.location.hash.match(/(?:^#|&)flux-launch=([^&]+)/);
      if (match) {
        const secret = decodeURIComponent(match[1]);
        window.history.replaceState(null, '', window.location.pathname + window.location.search);
        const exchange = await fetch('/api/workbench/exchange', {
          method: 'POST',
          credentials: 'same-origin',
          headers: {'content-type': 'application/json'},
          body: JSON.stringify({secret}),
        });
        if (!exchange.ok) throw new Error('The local docs launch link has expired. Restart `flux docs`.');
      }
      const response = await fetch('/api/workbench/bootstrap', {credentials: 'same-origin'});
      if (!response.ok) return {execution: false, lsp: false};
      return response.json();
    })().catch((error) => ({execution: false, lsp: false, error: error.message}));
  }
  return authPromise;
}

function registerFlux(monaco) {
  if (monaco.languages.getLanguages().some((language) => language.id === 'flux')) return;
  monaco.languages.register({id: 'flux', extensions: ['.flux']});
  monaco.languages.setMonarchTokensProvider('flux', {
    keywords: [
      'flow', 'op', 'agent', 'journey', 'channel', 'datasource', 'on', 'startup', 'user_input',
      'return', 'when', 'else', 'match', 'repeat', 'each', 'parallel', 'retry', 'catch', 'finally',
      'with', 'tools', 'risk', 'max', 'wait', 'allow', 'deny', 'true', 'false', 'null',
    ],
    tokenizer: {
      root: [
        [/#.*$/, 'comment'],
        [/"([^"\\]|\\.)*"/, 'string'],
        [/@[a-zA-Z_][\w.]*/, 'type.identifier'],
        [/\$[a-zA-Z_][\w.]*/, 'variable'],
        [/[a-zA-Z_][\w.]*/, {cases: {'@keywords': 'keyword', '@default': 'identifier'}}],
        [/\d+(?:\.\d+)?/, 'number'],
        [/[{}()[\],.:=|]/, 'delimiter'],
      ],
    },
  });
  monaco.languages.setLanguageConfiguration('flux', {
    comments: {lineComment: '#'},
    brackets: [['{', '}'], ['[', ']'], ['(', ')']],
    autoClosingPairs: [{open: '"', close: '"'}, {open: '(', close: ')'}, {open: '[', close: ']'}],
  });
}

class LspClient {
  constructor(monaco, model) {
    this.monaco = monaco;
    this.model = model;
    this.nextId = 0;
    this.pending = new Map();
    this.disposables = [];
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    this.socket = new WebSocket(`${protocol}//${window.location.host}/api/workbench/lsp`);
    this.socket.onmessage = ({data}) => this.receive(JSON.parse(data));
    this.ready = new Promise((resolve, reject) => {
      this.socket.onerror = () => reject(new Error('Flux LSP connection failed'));
      this.socket.onopen = async () => {
        await this.request('initialize', {
          processId: null,
          rootUri: 'file:///browser-cannot-select-this-root',
          capabilities: {textDocument: {completion: {}, hover: {}, publishDiagnostics: {}}},
          workspaceFolders: [{uri: 'file:///also-ignored', name: 'browser'}],
        });
        this.notify('initialized', {});
        this.notify('textDocument/didOpen', {textDocument: this.document(1)});
        resolve();
      };
    });
    this.installProviders();
  }

  document(version) {
    return {uri: this.model.uri.toString(), languageId: 'flux', version, text: this.model.getValue()};
  }

  request(method, params) {
    const id = ++this.nextId;
    this.socket.send(JSON.stringify({jsonrpc: '2.0', id, method, params}));
    return new Promise((resolve, reject) => this.pending.set(id, {resolve, reject}));
  }

  notify(method, params) {
    if (this.socket.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify({jsonrpc: '2.0', method, params}));
    }
  }

  receive(message) {
    if (message.id != null) {
      const pending = this.pending.get(message.id);
      if (pending) {
        this.pending.delete(message.id);
        message.error ? pending.reject(new Error(message.error.message)) : pending.resolve(message.result);
      }
      return;
    }
    if (message.method === 'textDocument/publishDiagnostics') {
      const markers = (message.params.diagnostics || []).map((diagnostic) => ({
        severity: diagnostic.severity === 1 ? this.monaco.MarkerSeverity.Error : this.monaco.MarkerSeverity.Warning,
        message: diagnostic.message,
        code: diagnostic.code,
        startLineNumber: diagnostic.range.start.line + 1,
        startColumn: diagnostic.range.start.character + 1,
        endLineNumber: diagnostic.range.end.line + 1,
        endColumn: diagnostic.range.end.character + 1,
      }));
      this.monaco.editor.setModelMarkers(this.model, 'flux-lsp', markers);
    }
  }

  position(position) {
    return {line: position.lineNumber - 1, character: position.column - 1};
  }

  range(range) {
    return new this.monaco.Range(
      range.start.line + 1, range.start.character + 1,
      range.end.line + 1, range.end.character + 1,
    );
  }

  installProviders() {
    this.disposables.push(this.monaco.languages.registerCompletionItemProvider('flux', {
      triggerCharacters: ['$', '@'],
      provideCompletionItems: async (model, position) => {
        if (model !== this.model) return {suggestions: []};
        await this.ready;
        const result = await this.request('textDocument/completion', {
          textDocument: {uri: model.uri.toString()}, position: this.position(position),
        });
        const items = Array.isArray(result) ? result : (result?.items || []);
        return {suggestions: items.map((item) => ({
          label: item.label,
          detail: item.detail,
          documentation: typeof item.documentation === 'string' ? item.documentation : item.documentation?.value,
          insertText: item.insertText || item.label,
          kind: this.monaco.languages.CompletionItemKind.Function,
          range: undefined,
        }))};
      },
    }));
    this.disposables.push(this.monaco.languages.registerHoverProvider('flux', {
      provideHover: async (model, position) => {
        if (model !== this.model) return null;
        await this.ready;
        const result = await this.request('textDocument/hover', {
          textDocument: {uri: model.uri.toString()}, position: this.position(position),
        });
        if (!result) return null;
        const content = result.contents?.value || result.contents || '';
        return {contents: [{value: String(content)}], range: result.range ? this.range(result.range) : undefined};
      },
    }));
    this.disposables.push(this.monaco.languages.registerDocumentFormattingEditProvider('flux', {
      provideDocumentFormattingEdits: async (model, options) => {
        if (model !== this.model) return [];
        await this.ready;
        const edits = await this.request('textDocument/formatting', {
          textDocument: {uri: model.uri.toString()}, options,
        });
        return (edits || []).map((edit) => ({range: this.range(edit.range), text: edit.newText}));
      },
    }));
  }

  changed(version) {
    this.notify('textDocument/didChange', {
      textDocument: {uri: this.model.uri.toString(), version},
      contentChanges: [{text: this.model.getValue()}],
    });
  }

  dispose() {
    this.notify('textDocument/didClose', {textDocument: {uri: this.model.uri.toString()}});
    this.socket.close();
    this.disposables.forEach((disposable) => disposable.dispose());
  }
}

function flattenNodes(nodes, depth = 0, output = []) {
  for (const node of nodes || []) {
    output.push({...node, depth});
    if (node.then) flattenNodes(node.then, depth + 1, output);
    if (node.otherwise) flattenNodes(node.otherwise, depth + 1, output);
    if (node.body) flattenNodes(node.body, depth + 1, output);
    for (const branch of node.branches || []) flattenNodes(branch.body, depth + 1, output);
  }
  return output;
}

export default function FluxWorkbench({source, fixture, expanded = false, title = 'Flux-Lang'}) {
  const workbenchId = useRef();
  if (!workbenchId.current) workbenchId.current = `workbench-${++nextWorkbenchId}`;
  const [open, setOpen] = useState(expanded);
  const [runtime, setRuntime] = useState({execution: false, lsp: false});
  const [code, setCode] = useState(source.trimEnd() + '\n');
  const [projection, setProjection] = useState(null);
  const [error, setError] = useState('');
  const [session, setSession] = useState(null);
  const [run, setRun] = useState(null);
  const [inputs, setInputs] = useState(JSON.stringify(FIXTURE_INPUTS[fixture] || {}, null, 2));
  const [appInput, setAppInput] = useState('What happens to my edits if I work offline?');
  const [activeFile, setActiveFile] = useState('main.flux');
  const [monacoReady, setMonacoReady] = useState(false);
  const editorHost = useRef(null);
  const editor = useRef(null);
  const models = useRef(new Map());
  const lsp = useRef(null);
  const version = useRef(1);
  const files = useMemo(() => ({'main.flux': code, ...(FIXTURE_FILES[fixture] || {})}), [fixture]);

  useEffect(() => { bootstrapRuntime().then(setRuntime); }, []);
  useEffect(() => {
    if (!open || !editorHost.current || editor.current) return undefined;
    let alive = true;
    import('monaco-editor').then((monaco) => {
      if (!alive) return;
      registerFlux(monaco);
      for (const [path, contents] of Object.entries(files)) {
        const language = path.endsWith('.flux') ? 'flux' : (path.endsWith('.rs') ? 'rust' : 'plaintext');
        models.current.set(path, monaco.editor.createModel(contents, language, monaco.Uri.parse(`file:///scratch/${workbenchId.current}/${path}`)));
      }
      editor.current = monaco.editor.create(editorHost.current, {
        model: models.current.get('main.flux'),
        theme: document.documentElement.dataset.theme === 'dark' ? 'vs-dark' : 'vs',
        automaticLayout: true,
        minimap: {enabled: false},
        fontSize: 14,
        lineNumbersMinChars: 3,
        scrollBeyondLastLine: false,
      });
      editor.current.onDidChangeModelContent(() => {
        if (editor.current.getModel() === models.current.get('main.flux')) {
          const value = editor.current.getValue();
          setCode(value);
          version.current += 1;
          lsp.current?.changed(version.current);
        }
      });
      if (runtime.lsp) {
        lsp.current = new LspClient(monaco, models.current.get('main.flux'));
        lsp.current.ready.catch((caught) => setError(caught.message));
      }
      setMonacoReady(true);
    }).catch((caught) => setError(`Editor failed to load: ${caught.message}`));
    return () => {
      alive = false;
      lsp.current?.dispose();
      editor.current?.dispose();
      for (const model of models.current.values()) model.dispose();
      models.current.clear();
    };
  }, [open, runtime.lsp]);

  const selectFile = useCallback((path) => {
    setActiveFile(path);
    editor.current?.setModel(models.current.get(path));
    editor.current?.updateOptions({readOnly: path !== 'main.flux'});
  }, []);

  const check = useCallback(async () => {
    setError('');
    try {
      const response = await fetch('/api/playground/project', {
        method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({source: code}),
      });
      const payload = await response.json();
      if (!response.ok) throw new Error(payload.error || `check failed (${response.status})`);
      setProjection(payload);
    } catch (caught) {
      setError(runtime.execution ? caught.message : 'Structural checking and execution are available in `flux docs`; this hosted page remains an editor.');
    }
  }, [code, runtime.execution]);

  const start = useCallback(async () => {
    setError('');
    try {
      if (session) {
        await fetch(`/api/workbench/sessions/${session.id}`, {method: 'DELETE', credentials: 'same-origin'});
      }
      const parsedInputs = JSON.parse(inputs);
      const create = await fetch('/api/workbench/sessions', {
        method: 'POST', credentials: 'same-origin', headers: {'content-type': 'application/json'},
        body: JSON.stringify({fixture, source: code, files: FIXTURE_FILES[fixture] || {}, inputs: parsedInputs}),
      });
      const prepared = await create.json();
      if (!create.ok) throw new Error(prepared.error || `preflight failed (${create.status})`);
      setSession(prepared);
      setProjection(prepared.projection);
      const response = await fetch(`/api/workbench/sessions/${prepared.id}/run`, {method: 'POST', credentials: 'same-origin'});
      if (!response.ok) throw new Error(`run failed to start (${response.status})`);
      setRun({status: 'running'});
    } catch (caught) {
      setError(caught.message);
    }
  }, [code, fixture, inputs, session]);

  useEffect(() => {
    if (!session || run?.status !== 'running') return undefined;
    const timer = window.setInterval(async () => {
      const response = await fetch(`/api/workbench/sessions/${session.id}`, {credentials: 'same-origin'});
      if (!response.ok) return;
      const status = await response.json();
      setRun(status);
      if (status.status !== 'running') window.clearInterval(timer);
    }, 600);
    return () => window.clearInterval(timer);
  }, [session, run?.status]);

  const decide = useCallback(async (approval, choice) => {
    const response = await fetch(`/api/workbench/sessions/${session.id}/approvals/${approval.id}`, {
      method: 'POST', credentials: 'same-origin', headers: {'content-type': 'application/json'},
      body: JSON.stringify({fingerprint: approval.fingerprint, choice}),
    });
    if (!response.ok) setError('That approval no longer matches the pending effect.');
  }, [session]);

  const cancel = useCallback(async () => {
    if (!session) return;
    await fetch(`/api/workbench/sessions/${session.id}/cancel`, {method: 'POST', credentials: 'same-origin'});
    setRun((current) => ({...current, status: 'cancelled'}));
  }, [session]);

  const sendAppInput = useCallback(async () => {
    if (!session) return;
    const response = await fetch(`/api/workbench/sessions/${session.id}/input`, {
      method: 'POST', credentials: 'same-origin', headers: {'content-type': 'application/json'},
      body: JSON.stringify({text: appInput}),
    });
    if (!response.ok) {
      setError(`app input was not accepted (${response.status})`);
      return;
    }
    setRun({status: 'running'});
  }, [appInput, session]);

  if (!open) {
    return <button className={styles.openButton} onClick={() => setOpen(true)}>Edit · Check{fixture ? ' · Run' : ''}</button>;
  }

  const nodes = flattenNodes(projection?.graph?.body);
  const canRun = Boolean(fixture && runtime.execution && runtime.runnable_fixtures?.includes(fixture));
  return (
    <section className={styles.workbench} aria-label={`${title} workbench`}>
      <header className={styles.header}>
        <div><strong>{title}</strong><span>{runtime.lsp ? 'Flux LSP connected' : 'syntax editor'}</span></div>
        <div className={styles.actions}>
          <button onClick={check}>Check</button>
          {canRun && <button className={styles.runButton} onClick={start} disabled={run?.status === 'running'}>▶ {fixture?.startsWith('first-app-') ? 'Start app' : 'Run'}</button>}
          {run?.status === 'running' && <button onClick={cancel}>Stop</button>}
        </div>
      </header>
      <div className={styles.fileTabs}>
        {Object.keys(files).map((path) => <button key={path} className={path === activeFile ? styles.activeFile : ''} onClick={() => selectFile(path)}>{path}</button>)}
      </div>
      <div ref={editorHost} className={styles.editor} aria-label="Flux source editor" />
      {!monacoReady && <div className={styles.loading}>Loading editor…</div>}
      {fixture && !fixture.startsWith('first-app-') && <details className={styles.inputs}><summary>Run inputs (JSON)</summary><textarea value={inputs} onChange={(event) => setInputs(event.target.value)} /></details>}
      {fixture?.startsWith('first-app-') && session && run?.status !== 'running' && <div className={styles.appInput}><input value={appInput} onChange={(event) => setAppInput(event.target.value)} /><button className={styles.runButton} onClick={sendAppInput}>Send</button></div>}
      {fixture?.startsWith('first-app-') && <p className={styles.posture}>This app session persists across messages. Editing or switching tutorial variants requires Start app to create a fresh session.</p>}
      {!runtime.execution && <p className={styles.posture}>Hosted mode: edit and syntax highlighting only. Run <code>flux docs</code> for LSP, scratch execution, and approvals.</p>}
      {session?.risk && <p className={styles.risk}><strong>Preflight:</strong> {session.risk.summary} · {session.risk.ops.join(', ') || 'no operations'}</p>}
      {error && <pre className={styles.error}>{error}</pre>}
      {run?.approvals?.map((approval) => (
        <div className={styles.approval} key={approval.id}>
          <div><strong>Approve {approval.tool}?</strong><small>{approval.summary || approval.subjects?.join(', ')}</small></div>
          <button onClick={() => decide(approval, 'deny')}>Deny</button>
          <button className={styles.runButton} onClick={() => decide(approval, 'allow')}>Allow once</button>
        </div>
      ))}
      {run?.events?.length > 0 && <pre className={styles.output}>{run.events.map((event) => event.kind === 'text' ? event.text : event.kind === 'tool_call' ? `→ ${event.name} ${JSON.stringify(event.input)}\n` : `← ${event.name}: ${event.content}\n`).join('')}</pre>}
      {(run?.result || run?.error) && <pre className={run.error ? styles.error : styles.output}>{run.error || run.result}</pre>}
      {projection && <div className={styles.graph}>
        <div className={styles.graphTitle}>Structure <span>{nodes.length} nodes</span></div>
        {nodes.map((node, index) => <div className={styles.node} style={{marginLeft: node.depth * 18}} key={node.id || `${node.kind}-${index}`}><span>{String(index + 1).padStart(2, '0')}</span><strong>{node.kind === 'call' ? `${node.op}()` : node.kind}</strong></div>)}
        {!projection.graph && <p>Source-only: {(projection.diagnostics || []).map((item) => item.message).join(' · ')}</p>}
      </div>}
    </section>
  );
}
