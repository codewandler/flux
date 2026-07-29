import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';
import CodeBlock from '@theme/CodeBlock';
import ThemedImage from '@theme/ThemedImage';
import useBaseUrl from '@docusaurus/useBaseUrl';

const HERO_FLOW = `flow triage-failures -> String
  $status = git_status()
  $tests  = cargo_test({args: ["--workspace"]})

  ctx $pack
    purpose "explain the failing tests"
    budget 9000
    include $status, $tests

  $diagnosis = ai.reason({ask: "Likely cause?", ctx: $pack})
  return $diagnosis`;

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
                The model compiles a request into a typed Flux-Lang plan. A Rust runtime executes
                that plan through authorization, approval, and guarded IO.
              </p>
              <div className="hero-actions">
                <Link className="button button--primary button--lg" to="/docs/tutorial">
                  Start the tutorial
                </Link>
                <Link className="button button--secondary button--lg" to="/docs/language/overview">
                  Explore Flux-Lang
                </Link>
              </div>
            </div>
            <div className="hero-code">
              <CodeBlock language="flux" title="triage.flux">
                {HERO_FLOW}
              </CodeBlock>
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
