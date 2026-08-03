import React, {useCallback, useEffect, useRef, useState} from 'react';
import Link from '@docusaurus/Link';
import CodeBlock from '@theme/CodeBlock';
import FluxWorkbench from '@site/src/components/FluxWorkbench';
import styles from './styles.module.css';

const SLIDES = [
  {eyebrow: '01 · orientation', title: 'What is flux?'},
  {eyebrow: '02 · the problem', title: 'A transcript is not a runtime contract'},
  {eyebrow: '03 · the execution path', title: 'Judgment proposes. The runtime decides.'},
  {eyebrow: '04 · run the real thing', title: 'Authored flow, guarded scratch'},
  {eyebrow: '05 · one substrate', title: 'Use it at the surface you need'},
  {eyebrow: '06 · vendor vocabulary', title: 'Connectors describe. Hosts execute.'},
  {eyebrow: '07 · shared authority', title: 'Exchange holds credentials, not the agent'},
  {eyebrow: '08 · topology', title: 'Core Flux stays useful without Exchange'},
  {eyebrow: '09 · SRE truth', title: 'Know the boundary—and the gaps'},
  {eyebrow: '10 · next step', title: 'Evaluate it from your own machine'},
];

const PIPELINE = [
  ['Typed intent', 'Classify the request and expose only relevant capabilities.'],
  ['Evidence', 'Read bounded, attributable facts before proposing an effect.'],
  ['Action batch', 'Freeze literal calls and their declared subjects before execution.'],
  ['Authorization', 'Intersect caller identity, policy, and the program permission ceiling.'],
  ['Approval', 'Ask a human when policy does not already decide.'],
  ['Guarded IO', 'All filesystem, process, and network effects cross one host-owned boundary.'],
];

const RUST_FILES = `flow rust-files(dirs: List<String>)
  each dir in dirs -> flat files
    glob(path: dir, pattern: "*.rs")
  each f in files -> stats
    file_stat(f)
  return { files, stats }
`;

function Card({title, children, tone = 'plain'}) {
  return (
    <article className={`${styles.card} ${styles[`card_${tone}`]}`}>
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
        <p>{slide.eyebrow}</p>
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
        <span>In 15 minutes</span>
        <ol>
          <li>Trace an effect from request to guarded IO.</li>
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

function DemoSlide() {
  return (
    <div className={styles.demo}>
      <p className={styles.demoIntro}>
        This is the same declared fixture used by the Flux-Lang console. In hosted docs it is an
        editor; from loopback <code>flux docs</code> it runs through the real dispatcher in an
        isolated scratch workspace.
      </p>
      <FluxWorkbench
        expanded
        title="Rust files · scratch fixture"
        fixture="rust-files"
        source={RUST_FILES}
      />
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
        <span>Source snapshot · 2026-08-03</span>
        <strong>flux-connectors main · v0.17.0</strong>
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
        executor; Flux will embed one client rather than host connector runtimes.
      </p>
    </div>
  );
}

function ExchangeSlide() {
  return (
    <div className={styles.exchangeLayout}>
      <div className={styles.snapshot}>
        <span>Source snapshot · 2026-08-03</span>
        <strong>flux-exchange main · v0.16.0</strong>
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
        <Card title="Inbound + history"><Status state="partial">partial</Status><p>Generated socket subscriptions and workflow activity ship; general lifecycle and execution records remain direction.</p></Card>
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
      <div className={styles.topology} aria-live="polite">
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
            <p><strong>Today:</strong> Exchange can invoke granted HTTP connector operations for a signed-in tenant.</p>
            <p><strong>Ships:</strong> Flux embeds the Exchange client and projects one Service Account&apos;s effective catalogue between turns.</p>
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
          <li>Flux ↔ Exchange effective-catalogue and invocation wiring is not built.</li>
          <li>Rich outbound runtime dispatch remains planned.</li>
          <li>The general stream/lease protocol is not built.</li>
          <li>Flux connector webhook support is deliberately narrow and unsigned-only today.</li>
          <li>OS process sandboxing is defense in depth and platform-dependent.</li>
        </ul>
      </div>
      <p className={styles.wideCallout}>For SREs, an explicit refusal is a feature: unavailable paths fail closed instead of changing locality or authority.</p>
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

function PresentationSlide({index, pipelineStage, setPipelineStage, topology, setTopology}) {
  const content = [
    <IntroSlide key="intro" />,
    <ProblemSlide key="problem" />,
    <PipelineSlide key="pipeline" active={pipelineStage} setActive={setPipelineStage} />,
    <DemoSlide key="demo" />,
    <SurfacesSlide key="surfaces" />,
    <ConnectorsSlide key="connectors" />,
    <ExchangeSlide key="exchange" />,
    <TopologySlide key="topology" topology={topology} setTopology={setTopology} />,
    <OperationsSlide key="operations" />,
    <NextSlide key="next" />,
  ];
  return <SlideFrame index={index}>{content[index]}</SlideFrame>;
}

function slideFromHash() {
  if (typeof window === 'undefined') return 0;
  const match = window.location.hash.match(/^#slide-(\d+)$/);
  return match ? Math.min(Math.max(Number(match[1]) - 1, 0), SLIDES.length - 1) : 0;
}

export default function FluxPresentation() {
  const deckRef = useRef(null);
  const [index, setIndex] = useState(0);
  const [pipelineStage, setPipelineStage] = useState(0);
  const [topology, setTopology] = useState('local');
  const [fullscreen, setFullscreen] = useState(false);

  const go = useCallback((next) => {
    const bounded = Math.min(Math.max(next, 0), SLIDES.length - 1);
    setIndex(bounded);
    if (typeof window !== 'undefined') {
      window.history.replaceState(null, '', `${window.location.pathname}${window.location.search}#slide-${bounded + 1}`);
    }
  }, []);

  useEffect(() => {
    setIndex(slideFromHash());
    const restore = () => setIndex(slideFromHash());
    window.addEventListener('hashchange', restore);
    return () => window.removeEventListener('hashchange', restore);
  }, []);

  useEffect(() => {
    const onKey = (event) => {
      const target = event.target;
      if (target instanceof HTMLElement && (
        target.isContentEditable ||
        ['A', 'BUTTON', 'INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) ||
        target.closest('.monaco-editor')
      )) return;
      const forward = ['ArrowRight', 'PageDown', ' '];
      const backward = ['ArrowLeft', 'PageUp'];
      if (forward.includes(event.key)) { event.preventDefault(); go(index + 1); }
      else if (backward.includes(event.key)) { event.preventDefault(); go(index - 1); }
      else if (event.key === 'Home') { event.preventDefault(); go(0); }
      else if (event.key === 'End') { event.preventDefault(); go(SLIDES.length - 1); }
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
    if (document.fullscreenElement) await document.exitFullscreen();
    else if (deckRef.current?.requestFullscreen) await deckRef.current.requestFullscreen();
  };

  return (
    <main className={styles.page}>
      <div className={styles.deck} ref={deckRef}>
        <div className={styles.progress} aria-hidden="true">
          <i style={{width: `${((index + 1) / SLIDES.length) * 100}%`}} />
        </div>
        <div className={styles.stage} aria-live="polite">
          <PresentationSlide
            index={index}
            pipelineStage={pipelineStage}
            setPipelineStage={setPipelineStage}
            topology={topology}
            setTopology={setTopology}
          />
        </div>
        <nav className={styles.controls} aria-label="Presentation controls">
          <span>{String(index + 1).padStart(2, '0')} / {SLIDES.length}</span>
          <div>
            <button type="button" onClick={() => go(index - 1)} disabled={index === 0} aria-label="Previous slide">←</button>
            <button type="button" onClick={toggleFullscreen} aria-label={fullscreen ? 'Exit fullscreen' : 'Enter fullscreen'}>{fullscreen ? 'Exit full' : 'Full screen'}</button>
            <button type="button" onClick={() => go(index + 1)} disabled={index === SLIDES.length - 1} aria-label="Next slide">→</button>
          </div>
          <small>← → · space · home/end</small>
        </nav>
      </div>
    </main>
  );
}
