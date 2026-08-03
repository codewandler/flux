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
    // Triple-quoted verbatim strings (L-39) must come first: they span lines, so the
    // single-line `string` pattern below cannot match them, and `greedy` lets the block
    // reclaim any `#` inside it that `comment` matched first (a `#` in a `"""` block is
    // content, not a comment). Same arrangement Prism's own Python grammar uses.
    'triple-quoted-string': {
      pattern: /"""[\s\S]*?"""/,
      greedy: true,
      alias: 'string',
      inside: {
        interpolation: {
          pattern: /\{[a-z_][a-z0-9_]*\}/,
          alias: 'variable',
        },
      },
    },
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
      /\b(?:flow|op|permissions|agent|channel|datasource|trigger|journey|goal|when|else|unless|match|route|case|default|fallback|branch|parallel|each|in|repeat|loop|seq|retry|timeout|budget|with_tools|assert|return|until|ctx|include|exclude|purpose|do|backoff|delay|for|every|flat|secret|description|risk|idempotency|effects|limits|expose|view|memo|once|checkpoint|await|confirm|throttle|debounce|verify|peek|try|catch|race|scope|saga|pipe|thing|finally|step|undo|per|max|wait|contains)\b/,
    // Quoted bracket keys remain separate `string` tokens; punctuation supplies the brackets.
    // Numeric indexes can stay part of a single variable token without hiding string semantics.
    variable: /\$[a-z_][a-z0-9_]*(?:(?:\.[A-Za-z0-9_]+)|(?:\[\d+\]))*/,
    boolean: /\b(?:true|false|null)\b/,
    number: /\b\d+(?:\.\d+)?\b/,
    function: /\b[a-z_][A-Za-z0-9_.]*(?=\()/,
    'class-name': /\b[A-Z][A-Za-z0-9_]*(?:<[A-Za-z0-9_<>]*>)?/,
    operator: /->|\+=|=|:/,
    punctuation: /[{}[\](),]/,
  };

  delete globalThis.Prism;
}
