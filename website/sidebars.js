// @ts-check

const sidebars = {
  docs: [
    'intro',
    'getting-started',
    'concepts',
    {
      type: 'category',
      label: 'Agent',
      items: [
        'agent/cli',
        'agent/agent-loop',
        'agent/providers',
        'agent/safety',
        'agent/a2a',
        'agent/programs',
      ],
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
          ],
        },
        'language/examples',
      ],
    },
    {
      type: 'category',
      label: 'SDK',
      items: ['sdk/flow-client'],
    },
    {
      type: 'category',
      label: 'Plugins',
      items: ['plugins/authoring'],
    },
    {
      type: 'category',
      label: 'Reference',
      items: ['reference/config'],
    },
  ],
};

module.exports = sidebars;
