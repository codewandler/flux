import React from 'react';
import Layout from '@theme/Layout';
import FluxPresentation from '@site/src/components/FluxPresentation';

export default function PresentationRoute() {
  return (
    <Layout
      title="What is flux?"
      description="A 15-minute interactive engineering presentation about flux, connectors, and Exchange.">
      <FluxPresentation />
    </Layout>
  );
}
