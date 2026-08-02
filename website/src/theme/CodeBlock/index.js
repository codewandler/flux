import React from 'react';
import OriginalCodeBlock from '@theme-original/CodeBlock';
import FluxWorkbench from '@site/src/components/FluxWorkbench';

function metadataValue(meta, name) {
  return meta?.match(new RegExp(`${name}=["']([^"']+)["']`))?.[1];
}

export default function CodeBlock(props) {
  const language = props.className?.replace(/^language-/, '') || props.language;
  if (language !== 'flux') return <OriginalCodeBlock {...props} />;
  const source = typeof props.children === 'string' ? props.children : String(props.children ?? '');
  const fixture = metadataValue(props.metastring, 'runnable');
  const title = metadataValue(props.metastring, 'title') || fixture || 'Flux-Lang';
  return <>
    <OriginalCodeBlock {...props} />
    <FluxWorkbench source={source} fixture={fixture} title={title} />
  </>;
}
