import siteConfig from '@generated/docusaurus.config';

export default function prismIncludeLanguages(PrismObject) {
  const {
    themeConfig: {prism},
  } = siteConfig;
  const {additionalLanguages} = prism;

  globalThis.Prism = PrismObject;

  additionalLanguages.forEach((lang) => {
    // eslint-disable-next-line global-require, import/no-dynamic-require
    require(`prismjs/components/prism-${lang}`);
  });

  // Flux-Lang (.flux) — hand-written grammar for the text syntax.
  PrismObject.languages.flux = {
    comment: /#.*/,
    string: {
      pattern: /"(?:\\.|[^"\\\r\n])*"/,
      greedy: true,
      inside: {
        interpolation: {
          pattern: /\{[a-z_][a-z0-9_]*\}/,
          alias: 'variable',
        },
      },
    },
    annotation: {
      pattern: /@(?:json|effect)\b/,
      alias: 'important',
    },
    keyword:
      /\b(?:flow|op|agent|channel|datasource|trigger|journey|goal|when|else|unless|match|route|case|default|fallback|branch|parallel|each|in|repeat|loop|seq|retry|timeout|budget|with_tools|assert|return|until|ctx|include|exclude|purpose|do|backoff|delay|for|every|flat|secret|description|risk|idempotency|effects|limits|expose|view)\b/,
    variable: /\$[a-z_][a-z0-9_]*(?:\.[A-Za-z0-9_]+)*/,
    boolean: /\b(?:true|false|null)\b/,
    number: /\b\d+(?:\.\d+)?\b/,
    function: /\b[a-z_][A-Za-z0-9_.]*(?=\()/,
    'class-name': /\b[A-Z][A-Za-z0-9_]*(?:<[A-Za-z0-9_<>]*>)?/,
    operator: /->|\+=|=|:/,
    punctuation: /[{}[\](),]/,
  };

  delete globalThis.Prism;
}
