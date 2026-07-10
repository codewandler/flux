// @ts-check

const sidebars = {
  docs: [
    'intro',
    'getting-started',
    'whats-new',
    'concepts',
    'infrastructure',
    'troubleshooting',
    {
      type: 'category',
      label: 'Agent',
      items: [
        'agent/cli',
        'agent/agent-loop',
        'agent/providers',
        'agent/claude-code',
        'agent/safety',
        'agent/a2a',
        'agent/programs',
        'agent/datasources',
        'agent/endpoints',
        'agent/saved-flows',
        'agent/skills-and-roles',
        'agent/time-machine',
        'agent/cost',
        'agent/realtime',
      ],
    },
    {
      type: 'category',
      label: 'Improvement',
      items: ['agent/improvement'],
    },
    {
      type: 'category',
      label: 'Flux-Lang',
      collapsed: false,
      items: [
        'language/overview',
        'language/tour',
        {
          type: 'category',
          label: 'Guide',
          items: [
            'language/flows-and-syntax',
            'language/control-flow',
            'language/pure-data',
            'language/context-packs',
            'language/concurrency',
            'language/reliability',
            'language/durability',
            'language/execution-model',
            'language/modules-and-programs',
          ],
        },
        {
          type: 'category',
          label: 'Reference',
          items: [
            'language/node-reference',
            'language/types-and-effects',
            'language/ops',
            'language/tooling',
            'language/editors',
          ],
        },
        'language/examples',
      ],
    },
    {
      type: 'category',
      label: 'SDK',
      items: ['sdk/overview', 'sdk/flow-client'],
    },
    {
      type: 'category',
      label: 'Plugins',
      items: ['plugins/using-plugins', 'plugins/gitlab', 'plugins/slack', 'plugins/authoring'],
    },
    {
      type: 'category',
      label: 'Security',
      items: [
        'security/overview',
        'security/credentials',
        'security/plugin-sandbox',
        'security/plugin-trust',
        'security/server-auth',
        { type: 'ref', id: 'agent/safety' },
      ],
    },
    {
      type: 'category',
      label: 'Reference',
      items: ['reference/config', 'reference/storage'],
    },
  ],
};

module.exports = sidebars;
