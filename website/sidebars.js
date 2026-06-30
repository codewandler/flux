// @ts-check

const sidebars = {
  docs: [
    'intro',
    'getting-started',
    'concepts',
    {
      type: 'category',
      label: 'Agent',
      items: ['agent/cli', 'agent/providers'],
    },
    {
      type: 'category',
      label: 'Flux-Lang',
      items: [
        'language/overview',
        'language/text-syntax',
        'language/execution-semantics',
        'language/ast-reference',
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
