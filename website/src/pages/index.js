import React, {useEffect, useState} from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import CodeBlock from '@theme/CodeBlock';
import ThemedImage from '@theme/ThemedImage';
import useBaseUrl from '@docusaurus/useBaseUrl';

const HERO_FLOW = `flow triage-failures -> String
  status = git_status()
  tests = cargo_test(args: ["--workspace"])

  ctx pack
    purpose "explain the failing tests"
    budget 9000
    include status, tests

  diagnosis = ai.reason(ask: "Likely cause?", ctx: pack)
  return diagnosis
`;

const HERO_STEPS = [
  {kind: 'inspect', label: 'git_status()', detail: 'read workspace state'},
  {kind: 'verify', label: 'cargo_test()', detail: 'run through guarded IO'},
  {kind: 'scope', label: 'ctx pack', detail: 'freeze the evidence slice'},
  {kind: 'reason', label: 'ai.reason()', detail: 'typed model stage'},
  {kind: 'return', label: 'return diagnosis', detail: 'publish the result'},
];

function VersionBadge() {
  const [version, setVersion] = useState(null);
  useEffect(() => {
    const controller = new AbortController();
    fetch('/version', {signal: controller.signal})
      .then((response) => response.ok ? response.json() : Promise.reject())
      .then((body) => setVersion(body.version))
      .catch(() => {});
    return () => controller.abort();
  }, []);
  return version ? <span className="served-version">flux v{version} · served locally</span> : null;
}

function HeroDebugger() {
  const [cursor, setCursor] = useState(-1);
  const [playing, setPlaying] = useState(false);
  useEffect(() => {
    if (!playing) return undefined;
    const timer = window.setInterval(() => {
      setCursor((current) => {
        if (current >= HERO_STEPS.length - 1) {
          setPlaying(false);
          return current;
        }
        return current + 1;
      });
    }, 900);
    return () => window.clearInterval(timer);
  }, [playing]);

  const move = (next) => {
    setPlaying(false);
    setCursor(Math.max(-1, Math.min(next, HERO_STEPS.length - 1)));
  };
  const toggle = () => {
    if (!playing && cursor >= HERO_STEPS.length - 1) setCursor(-1);
    setPlaying((value) => !value);
  };

  return (
    <div className="hero-debugger">
      <div className="hero-code-toolbar">
        <span>structural trace</span>
        <div className="hero-debug-controls" role="group" aria-label="Flux debugger preview controls">
          <button type="button" onClick={() => move(-1)} aria-label="Rewind" title="Rewind">&lt;&lt;</button>
          <button type="button" onClick={toggle} aria-label={playing ? 'Pause' : 'Play'} title={playing ? 'Pause' : 'Play'}>
            {playing ? 'Ⅱ' : '▶'}
          </button>
          <button type="button" onClick={() => move(HERO_STEPS.length - 1)} aria-label="Jump to end" title="Jump to end">&gt;&gt;</button>
        </div>
      </div>
      <CodeBlock language="flux" title="triage.flux">
        {HERO_FLOW}
      </CodeBlock>
      <div className="hero-trace" aria-live="polite">
        {HERO_STEPS.map((step, index) => (
          <div
            className={`hero-trace-node${index === cursor ? ' is-active' : ''}${index < cursor ? ' is-done' : ''}`}
            key={step.label}>
            <span>{String(index + 1).padStart(2, '0')}</span>
            <div><strong>{step.label}</strong><small>{step.detail}</small></div>
          </div>
        ))}
      </div>
      <p className="hero-trace-note">Preview only — no operation runs from this page.</p>
    </div>
  );
}

function Card({title, children, to}) {
  return (
    <Link className="home-card" to={to}>
      <h2>{title}</h2>
      <p>{children}</p>
    </Link>
  );
}

export default function Home() {
  return (
    <Layout
      title="flux"
      description="A deterministic agent platform where the LLM is not the runtime.">
      <main>
        <section className="home-hero">
          <div className="container home-hero-inner">
            <div>
              <p className="eyebrow">deterministic agent platform</p>
              <VersionBadge />
              {/*
                The wordmark is the brand lockup from assets/flux-logo.svg, shipped as two
                explicit files rather than one prefers-color-scheme SVG: the source asset keys
                off the OS scheme, which would disagree with the site's own theme toggle.
                The alt text carries the accessible name for the h1.
              */}
              <h1 className="home-wordmark">
                <ThemedImage
                  alt="flux"
                  sources={{
                    light: useBaseUrl('/img/flux-logo-light.svg'),
                    dark: useBaseUrl('/img/flux-logo-dark.svg'),
                  }}
                />
              </h1>
              <p className="hero-copy">
                Provider-native typed stages interpret intent and propose literal calls inside an
                authored Flux-Lang loop. The host freezes effects into action batches and runs them
                through authorization, approval, and guarded IO. The default loop never asks the
                model for per-turn executable Flux.
              </p>
              <div className="hero-actions">
                <Link className="button button--primary button--lg" to="/docs/tutorial">
                  Start the tutorial
                </Link>
                <Link className="button button--secondary button--lg" to="/docs/language/overview">
                  Explore Flux-Lang
                </Link>
                <Link className="button button--secondary button--lg" to="/console/">
                  Open the playground
                </Link>
              </div>
            </div>
            <div className="hero-code">
              <HeroDebugger />
            </div>
          </div>
        </section>
        <section className="container home-grid">
          <Card title="Agent" to="/docs/agent/cli">
            A local-first coding agent with policy, approvals, sessions, skills, and provider routing.
          </Card>
          <Card title="Flux-Lang" to="/docs/language/tour">
            A readable plan language with guard rails, concurrency, and budgets — auditable before it
            runs. Take the ten-minute tour.
          </Card>
          <Card title="SDK" to="/docs/sdk/flow-client">
            Parse, analyze, optimize, and execute flows from Rust through the same safety envelope.
          </Card>
          <Card title="Security" to="/docs/security/overview">
            Authorization, approvals, guarded IO, plugin confinement, and an opt-in OS sandbox — what
            each one guarantees, and where the boundaries sit.
          </Card>
        </section>
      </main>
    </Layout>
  );
}
