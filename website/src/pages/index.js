import React from 'react';
import Layout from '@theme/Layout';
import Link from '@docusaurus/Link';

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
          <div className="container">
            <p className="eyebrow">deterministic agent platform</p>
            <h1>flux</h1>
            <p className="hero-copy">
              The model compiles a request into a typed Flux-Lang plan. A Rust runtime executes that
              plan through authorization, approval, and guarded IO.
            </p>
            <div className="hero-actions">
              <Link className="button button--primary button--lg" to="/docs/intro">
                Read the docs
              </Link>
              <Link className="button button--secondary button--lg" to="/docs/language/overview">
                Explore Flux-Lang
              </Link>
            </div>
          </div>
        </section>
        <section className="container home-grid">
          <Card title="Agent" to="/docs/agent/cli">
            A local-first coding agent with policy, approvals, sessions, skills, and provider routing.
          </Card>
          <Card title="Flux-Lang" to="/docs/language/text-syntax">
            A readable text form and JSON AST for plans that can be audited before they run.
          </Card>
          <Card title="SDK" to="/docs/sdk/flow-client">
            Parse, analyze, optimize, and execute flows from Rust through the same safety envelope.
          </Card>
        </section>
      </main>
    </Layout>
  );
}
