import React from 'react';
import Link from '@docusaurus/Link';
import Layout from '@theme/Layout';
import FluxWorkbench from '@site/src/components/FluxWorkbench';

export default function ConsoleRoute() {
  return (
    <Layout title="Flux-Lang workbench" description="Edit, check, and run Flux-Lang in guarded scratch projects.">
      <main className="container" style={{paddingTop: '2.5rem', paddingBottom: '4rem'}}>
        <h1>Flux-Lang workbench</h1>
        <p>Edit with real Flux syntax support and LSP diagnostics. The local <code>flux docs</code> surface can execute declared examples in isolated scratch projects; this hosted page remains an editor.</p>
        <p>
          <Link className="button button--secondary" to="/presentation/">
            Present flux to your engineering team
          </Link>
        </p>
        <FluxWorkbench
          expanded
          title="Rust files example"
          fixture="rust-files"
          source={`flow rust-files(dirs: List<String>)
  each dir in dirs -> flat files
    glob(path: dir, pattern: "*.rs")
  each f in files -> stats
    file_stat(f)
  return { files, stats }
`}
        />
      </main>
    </Layout>
  );
}
