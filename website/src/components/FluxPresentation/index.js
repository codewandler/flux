import React, {useCallback, useEffect, useRef, useState} from 'react';
import Link from '@docusaurus/Link';
import CodeBlock from '@theme/CodeBlock';
import FluxWorkbench from '@site/src/components/FluxWorkbench';
import styles from './styles.module.css';

const PIPELINE = [
  ['Typed intent', 'Classify the request and expose only relevant capabilities.'],
  ['Evidence', 'Read bounded, attributable facts before proposing an effect.'],
  ['Action batch', 'Freeze literal calls and their declared subjects before execution.'],
  ['Authorization', 'Intersect caller identity, policy, and the program permission ceiling.'],
  ['Approval', 'Ask a human when policy does not already decide.'],
  ['Guarded IO', 'All filesystem, process, and network effects cross one host-owned boundary.'],
];

const LOOP_STEPS = [
  ['detect_intent', 'Classify the request; intersect its signals with registered, wired, and permitted operations.'],
  ['explore', 'The model works those operations’ exact native schemas. Safe reads run and return evidence; effectful calls are captured, not executed.'],
  ['freeze', 'Captured calls become one immutable, ordered action batch.'],
  ['approve_batch', 'A one-shot receipt bound to batch, session, caller, and policy.'],
  ['execute_batch', 'Authorization, approval scope, then guarded IO. Failures return to the same ledger for local correction.'],
  ['present_results', 'The answer — while the session keeps the full typed record.'],
];

const RUST_FILES = `flow rust-files(dirs: List<String>)
  each dir in dirs -> flat files
    glob(path: dir, pattern: "*.rs")
  each f in files -> stats
    file_stat(f)
  return { files, stats }
`;

const LOOP_TRACE = `intent…
◆ intent: update the release notes
  capabilities: workspace.read, workspace.write
exploring…
`;

const MODEL_EXAMPLES = `flux run -m opus "…"                     # Anthropic alias
flux run -m openai/gpt-5 "…"             # provider/model, forwarded verbatim
flux run -m openrouter/z-ai/glm-4.6 "…"  # long-tail catalogue
flux run -m mock "…"                     # offline, deterministic
`;

function Card({title, children, tone = 'plain'}) {
  const toneClass = styles[`card_${tone}`];
  return (
    <article className={toneClass ? `${styles.card} ${toneClass}` : styles.card}>
      <h3>{title}</h3>
      {children}
    </article>
  );
}

function Status({children, state}) {
  return <span className={`${styles.status} ${styles[`status_${state}`]}`}>{children}</span>;
}

function SlideFrame({index, children}) {
  const slide = SLIDES[index];
  return (
    <section
      className={styles.slide}
      aria-label={`Slide ${index + 1} of ${SLIDES.length}: ${slide.title}`}
      aria-roledescription="slide">
      <header className={styles.slideHeader}>
        <p>{`${String(index + 1).padStart(2, '0')} · ${slide.eyebrow}`}</p>
        <h1>{slide.title}</h1>
      </header>
      <div className={styles.slideBody}>{children}</div>
    </section>
  );
}

function IntroSlide() {
  return (
    <div className={styles.introGrid}>
      <div>
        <p className={styles.lede}>
          A Rust agent SDK, harness, and authored workflow language built around one boundary:
        </p>
        <blockquote className={styles.thesis}>The LLM is not the runtime.</blockquote>
        <p className={styles.supporting}>
          Models supply bounded judgment. Authored control flow and a deterministic host own order,
          authority, effects, evidence, and stopping.
        </p>
      </div>
      <div className={styles.takeaways}>
        <span>In about 20 minutes</span>
        <ol>
          <li>Trace an effect from request to guarded IO.</li>
          <li>Follow one adaptive turn to its one-shot approval receipt.</li>
          <li>Run one real authored flow locally.</li>
          <li>Place connectors and Exchange on the right side of the boundary.</li>
        </ol>
        <p>For developers + SREs evaluating the system together.</p>
      </div>
    </div>
  );
}

function ProblemSlide() {
  return (
    <div className={styles.twoColumns}>
      <Card title="Transcript as runtime" tone="warning">
        <ul>
          <li>Control flow lives in prose.</li>
          <li>Authority is implicit in the available tools.</li>
          <li>Retries can repeat effects.</li>
          <li>“What happened?” requires reconstructing a conversation.</li>
        </ul>
      </Card>
      <Card title="Flux runtime" tone="signal">
        <ul>
          <li>Authored Flux-Lang owns order and bounds.</li>
          <li>Calls freeze into typed action batches.</li>
          <li>Caller identity and policy are explicit inputs.</li>
          <li>Sessions retain evidence, usage, approvals, and outcomes.</li>
        </ul>
      </Card>
      <p className={styles.wideCallout}>
        The model can be capable without becoming the process supervisor, policy engine, or IO
        implementation.
      </p>
    </div>
  );
}

function PipelineSlide({active, setActive}) {
  return (
    <div className={styles.pipelineLayout}>
      <div className={styles.pipeline} role="list" aria-label="Flux execution path">
        {PIPELINE.map(([name], index) => (
          <React.Fragment key={name}>
            <button
              type="button"
              className={index === active ? styles.pipelineActive : ''}
              onClick={() => setActive(index)}
              aria-pressed={index === active}>
              <span>{String(index + 1).padStart(2, '0')}</span>
              {name}
            </button>
            {index < PIPELINE.length - 1 && <i aria-hidden="true">→</i>}
          </React.Fragment>
        ))}
      </div>
      <div className={styles.pipelineDetail} aria-live="polite">
        <span>Step {active + 1}</span>
        <h2>{PIPELINE[active][0]}</h2>
        <p>{PIPELINE[active][1]}</p>
      </div>
      <div className={styles.envelope}>
        <span>Mandatory safety envelope</span>
        <strong>authorization → approval → guarded IO</strong>
        <small>No tool-side shortcut. No model-authored shell string.</small>
      </div>
    </div>
  );
}

function LoopSlide() {
  return (
    <div className={styles.loopLayout}>
      <ol className={styles.loopList} aria-label="One adaptive turn">
        {LOOP_STEPS.map(([name, detail]) => (
          <li key={name}>
            <strong>{name}</strong>
            <p>{detail}</p>
          </li>
        ))}
      </ol>
      <div className={styles.loopSide}>
        <Card title="Durable by construction" tone="signal">
          <p>
            A typed decision request parks on Flux-Lang’s ordinary <code>await</code>. The user’s
            next message resumes the exact flow — bindings and native-stage ledger intact, nothing
            reconstructed. Receipts are one-shot: changed, stale, reused, or cross-session batches
            are rejected.
          </p>
        </Card>
        <Card title="Why not model-generated plans?">
          <p>
            A one-shot generated graph asked the least reliable component to pick operations,
            reproduce their schemas in a second language, and invent all control flow before it had
            evidence. The adaptive loop keeps judgment in typed stages; order, bounds, and batch
            identity stay with the authored flow and the host.
          </p>
        </Card>
      </div>
      <p className={styles.wideCallout}>
        The model does not generate Flux code. It participates inside typed stages; authored
        Flux-Lang owns the sequence.
      </p>
    </div>
  );
}

function DemoSlide({mounted}) {
  return (
    <div className={styles.demo}>
      <p className={styles.demoIntro}>
        This is the same declared fixture used by the Flux-Lang console. In hosted docs it is an
        editor; from loopback <code>flux docs</code> it runs through the real dispatcher in an
        isolated scratch workspace.
      </p>
      <div className={styles.demoScreen}>
        {mounted ? (
          <FluxWorkbench
            expanded
            title="Rust files · scratch fixture"
            fixture="rust-files"
            source={RUST_FILES}
          />
        ) : (
          <CodeBlock language="flux">{RUST_FILES}</CodeBlock>
        )}
      </div>
      <div className={styles.printOnly}>
        <CodeBlock language="flux">{RUST_FILES}</CodeBlock>
      </div>
    </div>
  );
}

function SurfacesSlide() {
  return (
    <div className={styles.surfaceGrid}>
      <Card title="CLI + TUI"><p>Daily coding agent, approvals, sessions, replay, and evidence.</p></Card>
      <Card title="Flux-Lang"><p>Authored flows, programs, channels, triggers, budgets, and concurrency.</p></Card>
      <Card title="Rust SDK"><p>Embed the same parser, engine, dispatcher, and typed host contracts.</p></Card>
      <Card title="HTTP + A2A"><p>Serve authenticated agents or connect to another agent over a standard protocol.</p></Card>
      <Card title="Plugins"><p>Temporary compatibility integrations, removed after their Exchange replacements prove parity.</p></Card>
      <Card title="Remote system"><p>Keep identity and approval local while guarded effects land in a remote workspace.</p></Card>
      <p className={styles.wideCallout}>Different entry points; one execution substrate and one safety envelope.</p>
    </div>
  );
}

function ConnectorsSlide() {
  return (
    <div className={styles.ecosystemLayout}>
      <div className={styles.snapshot}>
        <span>Source snapshot · 2026-08-05</span>
        <strong>flux-connectors main · v0.20.0</strong>
      </div>
      <div className={styles.flowRow}>
        <div><small>author once</small><strong>provider TOML</strong></div>
        <b>→</b>
        <div><small>compile + review</small><strong>typed Flux</strong></div>
        <b>→</b>
        <div><small>publish</small><strong>catalogue + manifest + Tool pack</strong></div>
      </div>
      <div className={styles.twoColumns}>
        <Card title="What is vendor truth">
          <p>Base URLs, authentication shape, operations, inputs, risk, effects, idempotency, and declared inbound events.</p>
        </Card>
        <Card title="What stays host-owned">
          <p>Credential values, grants, guarded HTTP, private-network policy, audit evidence, and deployment.</p>
        </Card>
      </div>
      <p className={styles.truthLine}>
        Every official integration is connector-owned. Exchange is the only official integration
        executor; Flux embeds one client rather than hosting connector runtimes.
      </p>
    </div>
  );
}

function ExchangeSlide() {
  return (
    <div className={styles.exchangeLayout}>
      <div className={styles.snapshot}>
        <span>Source snapshot · 2026-08-05</span>
        <strong>flux-exchange main · v0.17.0</strong>
      </div>
      <blockquote className={styles.exchangeThesis}>
        The credential never crosses the boundary; the authority does.
      </blockquote>
      <div className={styles.statusGrid}>
        <Card title="Identity + tenancy"><Status state="ships">ships</Status><p>OIDC sign-in and tenant-scoped sessions.</p></Card>
        <Card title="Connections"><Status state="ships">ships</Status><p>Create, rotate, and delete tenant credentials and settings.</p></Card>
        <Card title="Metadata grants"><Status state="ships">ships</Status><p>Admit operations by connector, risk, effects, and idempotency—not maintained ID lists.</p></Card>
        <Card title="Invoke"><Status state="ships">ships</Status><p>Build and execute an admitted HTTP operation from its compiled connector definition.</p></Card>
        <Card title="Service Accounts"><Status state="ships">ships</Status><p>Canonical bearer authentication keeps the vendor credential behind Exchange.</p></Card>
        <Card title="Inbound sockets"><Status state="partial">partial</Status><p>Generated socket subscriptions and workflow activity ship.</p></Card>
        <Card title="Lifecycle + records"><Status state="direction">direction</Status><p>General inbound lifecycle and execution records are charter, not yet built.</p></Card>
      </div>
    </div>
  );
}

function TopologySlide({topology, setTopology}) {
  const shared = topology === 'shared';
  return (
    <div className={styles.topologyLayout}>
      <div className={styles.segmented} role="group" aria-label="Deployment topology">
        <button type="button" onClick={() => setTopology('local')} aria-pressed={!shared}>Local operator</button>
        <button type="button" onClick={() => setTopology('shared')} aria-pressed={shared}>Shared team</button>
      </div>
      <p className={styles.srOnly} role="status">
        {shared
          ? 'Shared team topology: caller, then flux, then flux-exchange, then the declared vendor API.'
          : 'Local operator topology: caller, then flux, then the guarded System, then the workspace or service.'}
      </p>
      <div className={styles.topology}>
        <div className={styles.actor}><small>caller</small><strong>{shared ? 'engineer or agent' : 'local engineer'}</strong></div>
        <b>→</b>
        <div className={styles.fluxNode}><small>judgment + control</small><strong>flux</strong><span>identity · policy · approval · evidence</span></div>
        <b>→</b>
        {shared ? (
          <div className={styles.exchangeNode}><small>shared authority</small><strong>flux-exchange</strong><span>tenant · credential · grant · invoke</span></div>
        ) : (
          <div className={styles.hostNode}><small>local effects</small><strong>guarded System</strong><span>workspace · process · network</span></div>
        )}
        <b>→</b>
        <div className={styles.vendorNode}><small>external effect</small><strong>{shared ? 'declared vendor API' : 'workspace / service'}</strong></div>
      </div>
      <div className={styles.topologyNotes}>
        {shared ? (
          <>
            <p><strong>Today:</strong> Flux embeds the Exchange client: one Service Account&apos;s effective catalogue is projected between turns, and admitted operations invoke through Exchange.</p>
            <p><strong>Direction:</strong> streaming invocation, general inbound lifecycle, and execution records land Exchange-side; Flux keeps one client seam.</p>
          </>
        ) : (
          <>
            <p><strong>Today:</strong> Flux is complete as a local agent, workflow engine, SDK, and guarded effect host.</p>
            <p><strong>Rule:</strong> Core Flux remains useful without Exchange; official external integrations do not.</p>
          </>
        )}
      </div>
    </div>
  );
}

function SessionsSlide() {
  return (
    <div className={styles.sessionsLayout}>
      <div>
        <p className={styles.lede}>
          Every run persists a session: evidence, model calls, usage, approval outcomes, and results
          — replayable after the fact.
        </p>
        <CodeBlock language="text">{LOOP_TRACE}</CodeBlock>
        <p className={styles.supporting}>
          <code>--show-loop</code> prints one line per model call — stage, round, wall time, TTFT,
          operation count, schema size. The same redacted data is stored as <code>model.call</code>{' '}
          evidence with session and turn correlation; <code>/evidence</code> shows the audit trail.
        </p>
      </div>
      <div className={styles.sessionsCards}>
        <Card title="Approvals leave receipts">
          <p>
            Approve with <code>y</code> once or <code>a</code> always (saved to config), or deny.
            <code>--yes</code> installs a headless approver for trusted unattended work — and never
            overrides an authorization denial. Outcomes carry wait time; executed batches carry
            duration.
          </p>
        </Card>
        <Card title="Redaction is registered">
          <p>
            Secrets are registered with the redactor and scrubbed from tool output, logs, and stored
            evidence — not pattern-guessed after the fact.
          </p>
        </Card>
      </div>
      <p className={styles.wideCallout}>
        Sessions are the record: “what happened?” is a query over typed records, not a re-read of a
        conversation.
      </p>
    </div>
  );
}

function OperationsSlide() {
  return (
    <div className={styles.opsLayout}>
      <div className={styles.opsColumn}>
        <h2>Release-blocking invariants</h2>
        <ul className={styles.checkList}>
          <li>Caller identity is immutable for a live turn.</li>
          <li>Every real effect crosses guarded IO.</li>
          <li>Secrets are registered for redaction, never surfaced raw.</li>
          <li>Network destinations are resolved and private ranges gated.</li>
          <li>Session termination preserves provider-valid history.</li>
        </ul>
      </div>
      <div className={styles.opsColumn}>
        <h2>Plan around these gaps</h2>
        <ul className={styles.gapList}>
          <li>Exchange integration is a one-shot HTTP invoke behind one Service Account; streaming invocation and the general stream/lease protocol are not built.</li>
          <li>Rich outbound runtime dispatch remains planned.</li>
          <li>General Exchange inbound lifecycle and execution records remain direction.</li>
          <li>Connector inbound support is narrow by design: declarative webhooks and generated socket subscriptions, unsigned-only until the HMAC verifier ships.</li>
          <li>OS process sandboxing is defense in depth and platform-dependent.</li>
        </ul>
      </div>
      <p className={styles.wideCallout}>For SREs, an explicit refusal is a feature: unavailable paths fail closed instead of changing locality or authority.</p>
    </div>
  );
}

function ModelsSlide() {
  return (
    <div className={styles.modelsLayout}>
      <p className={styles.lede}>
        A provider is a wire codec and a credential lifecycle; the model string after it goes to the
        provider verbatim. Judgment is swappable because authority never lives in the model.
      </p>
      <CodeBlock language="bash">{MODEL_EXAMPLES}</CodeBlock>
      <div className={styles.modelCards}>
        <Card title="Offline by default" tone="signal">
          <p>
            <code>mock</code> is the deterministic offline provider: no key, no network, and it
            exercises the full pipeline — evaluate the machinery in CI or air-gapped.
          </p>
        </Card>
        <Card title="Local to cloud">
          <p>
            Eight production prefixes over four wire codecs: API keys, subscription OAuth (Claude,
            Codex), Bedrock, local Ollama, and OpenRouter&apos;s long-tail catalogue.
          </p>
        </Card>
        <Card title="No surprise egress">
          <p>
            A sub-agent always runs on its parent&apos;s provider; a role naming a different
            provider fails at spawn time, not mid-turn.
          </p>
        </Card>
      </div>
    </div>
  );
}

function NextSlide() {
  return (
    <div className={styles.nextLayout}>
      <div>
        <p className={styles.lede}>Start offline. Inspect the machinery. Then choose where effects should live.</p>
        <CodeBlock language="bash">{`# Full loop, no provider credential or network
flux run -m mock "inspect this repository and explain its test posture"

# Release-matched docs + guarded scratch examples
flux docs

# Reveal typed stages and the action-batch machinery
flux run -m mock --show-loop "summarize the architecture"`}</CodeBlock>
      </div>
      <div className={styles.resourceList}>
        <span>Keep exploring</span>
        <Link to="/docs/getting-started">Getting started</Link>
        <Link to="/docs/concepts">Concepts and execution model</Link>
        <Link to="/docs/ecosystem">Flux, connectors, and Exchange</Link>
        <Link to="/docs/security/overview">Security boundaries</Link>
        <a href="https://github.com/codewandler/flux-connectors">Connector inventory ↗</a>
        <a href="https://github.com/codewandler/flux-exchange">Exchange inventory ↗</a>
      </div>
    </div>
  );
}

const SLIDES = [
  {key: 'intro', eyebrow: 'orientation', title: 'What is flux?', render: () => <IntroSlide />},
  {key: 'problem', eyebrow: 'the problem', title: 'A transcript is not a runtime contract', render: () => <ProblemSlide />},
  {key: 'pipeline', eyebrow: 'the execution path', title: 'Judgment proposes. The runtime decides.', render: (p) => <PipelineSlide active={p.pipelineStage} setActive={p.setPipelineStage} />},
  {key: 'loop', eyebrow: 'the agent loop', title: 'Adaptive turns, deterministic seams', render: () => <LoopSlide />},
  {key: 'demo', eyebrow: 'run the real thing', title: 'Authored flow, guarded scratch', render: (p) => <DemoSlide mounted={p.demoMounted} />},
  {key: 'surfaces', eyebrow: 'one substrate', title: 'Use it at the surface you need', render: () => <SurfacesSlide />},
  {key: 'connectors', eyebrow: 'vendor vocabulary', title: 'Connectors describe. Hosts execute.', render: () => <ConnectorsSlide />},
  {key: 'exchange', eyebrow: 'shared authority', title: 'Exchange holds credentials, not the agent', render: () => <ExchangeSlide />},
  {key: 'topology', eyebrow: 'topology', title: 'Core Flux stays useful without Exchange', render: (p) => <TopologySlide topology={p.topology} setTopology={p.setTopology} />},
  {key: 'sessions', eyebrow: 'sessions', title: 'Sessions are the record', render: () => <SessionsSlide />},
  {key: 'operations', eyebrow: 'SRE truth', title: 'Know the boundary—and the gaps', render: () => <OperationsSlide />},
  {key: 'models', eyebrow: 'model strategy', title: 'Any provider. Same envelope.', render: () => <ModelsSlide />},
  {key: 'next', eyebrow: 'next step', title: 'Evaluate it from your own machine', render: () => <NextSlide />},
];

function slideFromHash() {
  if (typeof window === 'undefined') return 0;
  const match = window.location.hash.match(/^#slide-(\d+)$/);
  return match ? Math.min(Math.max(Number(match[1]) - 1, 0), SLIDES.length - 1) : 0;
}

export default function FluxPresentation() {
  const deckRef = useRef(null);
  const pointerStart = useRef(null);
  const [index, setIndex] = useState(0);
  const [pipelineStage, setPipelineStage] = useState(0);
  const [topology, setTopology] = useState('local');
  const [fullscreen, setFullscreen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [demoMounted, setDemoMounted] = useState(false);

  const go = useCallback((next) => {
    const bounded = Math.min(Math.max(next, 0), SLIDES.length - 1);
    setIndex(bounded);
    setMenuOpen(false);
    if (typeof window !== 'undefined' && slideFromHash() !== bounded) {
      window.history.pushState(null, '', `${window.location.pathname}${window.location.search}#slide-${bounded + 1}`);
    }
  }, []);

  useEffect(() => {
    setIndex(slideFromHash());
    const restore = () => setIndex(slideFromHash());
    window.addEventListener('hashchange', restore);
    return () => window.removeEventListener('hashchange', restore);
  }, []);

  // The workbench (Monaco + runtime bootstrap) mounts once the demo chapter is first shown and
  // stays mounted; server and initial client render agree on the static listing.
  useEffect(() => {
    if (SLIDES[index].key === 'demo') setDemoMounted(true);
  }, [index]);

  useEffect(() => {
    const onKey = (event) => {
      const target = event.target;
      // Editors and form fields own their keys entirely.
      if (target instanceof HTMLElement && (
        target.isContentEditable ||
        ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) ||
        target.closest('.monaco-editor')
      )) return;
      // A focused button or link keeps Space (activation); arrows still drive the deck.
      const activatable = target instanceof HTMLElement &&
        ['A', 'BUTTON', 'SUMMARY'].includes(target.tagName);
      if (event.key === ' ') {
        if (activatable) return;
        event.preventDefault();
        go(index + 1);
      } else if (event.key === 'ArrowRight' || event.key === 'PageDown') { event.preventDefault(); go(index + 1); }
      else if (event.key === 'ArrowLeft' || event.key === 'PageUp') { event.preventDefault(); go(index - 1); }
      else if (event.key === 'Home') { event.preventDefault(); go(0); }
      else if (event.key === 'End') { event.preventDefault(); go(SLIDES.length - 1); }
      else if (event.key === 'Escape') { setMenuOpen(false); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [go, index]);

  useEffect(() => {
    const changed = () => setFullscreen(document.fullscreenElement === deckRef.current);
    document.addEventListener('fullscreenchange', changed);
    return () => document.removeEventListener('fullscreenchange', changed);
  }, []);

  const toggleFullscreen = async () => {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else if (deckRef.current?.requestFullscreen) await deckRef.current.requestFullscreen();
    } catch {
      // Fullscreen can be denied (iframe, permission policy); the deck keeps working inline.
    }
  };

  const onPointerDown = (event) => {
    if (event.pointerType === 'mouse') return;
    if (event.target instanceof HTMLElement && event.target.closest('.monaco-editor, .workbench')) return;
    pointerStart.current = {x: event.clientX, y: event.clientY};
  };

  const onPointerUp = (event) => {
    const start = pointerStart.current;
    pointerStart.current = null;
    if (!start) return;
    const dx = event.clientX - start.x;
    const dy = event.clientY - start.y;
    if (Math.abs(dx) >= 48 && Math.abs(dx) > 2 * Math.abs(dy)) go(index + (dx < 0 ? 1 : -1));
  };

  const slideProps = {pipelineStage, setPipelineStage, topology, setTopology, demoMounted};

  return (
    <main className={styles.page}>
      <div className={styles.deck} ref={deckRef}>
        <div
          className={styles.progress}
          role="progressbar"
          aria-label="Slide progress"
          aria-valuemin={1}
          aria-valuemax={SLIDES.length}
          aria-valuenow={index + 1}>
          <i style={{width: `${((index + 1) / SLIDES.length) * 100}%`}} aria-hidden="true" />
        </div>
        <div className={styles.srOnly} role="status">
          {`Slide ${index + 1} of ${SLIDES.length}: ${SLIDES[index].title}`}
        </div>
        <div className={styles.stage} onPointerDown={onPointerDown} onPointerUp={onPointerUp}>
          {SLIDES.map((slide, i) => (
            <div
              key={slide.key}
              className={i === index ? styles.slideWrap : `${styles.slideWrap} ${styles.slideHidden}`}
              aria-hidden={i !== index}
              inert={i !== index || undefined}>
              <SlideFrame index={i}>{slide.render(slideProps)}</SlideFrame>
            </div>
          ))}
        </div>
        <nav className={styles.controls} aria-label="Presentation controls">
          <span>{String(index + 1).padStart(2, '0')} / {SLIDES.length}</span>
          <div>
            <div className={styles.menuWrap}>
              <button
                type="button"
                onClick={() => setMenuOpen((open) => !open)}
                aria-expanded={menuOpen}
                aria-haspopup="true">
                Contents
              </button>
              {menuOpen && (
                <div className={styles.menu} role="menu" aria-label="Jump to slide">
                  {SLIDES.map((slide, i) => (
                    <button
                      key={slide.key}
                      type="button"
                      role="menuitem"
                      className={i === index ? styles.menuActive : undefined}
                      onClick={() => go(i)}>
                      <span>{String(i + 1).padStart(2, '0')}</span> {slide.title}
                    </button>
                  ))}
                </div>
              )}
            </div>
            <button type="button" onClick={() => go(index - 1)} disabled={index === 0} aria-label="Previous slide">←</button>
            <button type="button" onClick={toggleFullscreen} aria-label={fullscreen ? 'Exit fullscreen' : 'Enter fullscreen'}>{fullscreen ? 'Exit full' : 'Full screen'}</button>
            <button type="button" onClick={() => go(index + 1)} disabled={index === SLIDES.length - 1} aria-label="Next slide">→</button>
          </div>
          <small>← → · space · swipe · home/end</small>
        </nav>
      </div>
    </main>
  );
}
