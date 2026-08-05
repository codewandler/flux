// @ts-check

const sidebars = {
  docs: [
    'intro',
    {
      type: 'category',
      label: 'Start here',
      collapsed: false,
      items: [
        'getting-started',
        {
          type: 'category',
          label: 'Tutorial',
          collapsed: false,
          items: [
            'tutorial',
            'tutorial/first-agent',
            'tutorial/first-flow',
            'tutorial/first-app',
          ],
        },
      ],
    },
    'whats-new',
    {
      type: 'category',
      label: 'Coding / AI-assisted development',
      collapsed: false,
      items: [
        'coding/overview',
        'coding/boards',
        'coding/fleet',
      ],
    },
    {
      type: 'category',
      label: 'Fundamentals',
      items: ['concepts', 'ecosystem', 'infrastructure', 'topologies', 'remote-system-deployment'],
    },
    {
      type: 'category',
      label: 'Agent',
      items: [
        {
          type: 'category',
          label: 'Run the coding agent',
          items: [
            'agent/cli',
            'agent/tui',
            'agent/providers',
            'agent/claude-code',
            'agent/project-context',
            'agent/context-management',
            'agent/skills-and-roles',
            'agent/claude-compat',
          ],
        },
        {
          type: 'category',
          label: 'Build applications',
          items: [
            'agent/programs',
            'agent/saved-flows',
            'agent/datasources',
          ],
        },
        {
          type: 'category',
          label: 'Serve and connect',
          items: [
            'agent/http-api',
            'agent/a2a',
            'agent/a2a-conformance',
            'agent/endpoints',
            'agent/realtime',
          ],
        },
        {
          type: 'category',
          label: 'Operate and inspect',
          items: [
            'agent/agent-loop',
            'agent/safety',
            'agent/time-machine',
            'agent/cost',
          ],
        },
      ],
    },
    {
      type: 'category',
      label: 'Integrations',
      items: [
        {
          type: 'category',
          label: 'Channels',
          items: [
            'channels/overview',
            'channels/inventory',
            'channels/connector',
            'agent/slack-channel',
          ],
        },
        {
          type: 'category',
          label: 'Plugins',
          items: [
            'plugins/using-plugins',
            'plugins/gitlab',
            'plugins/slack',
            'plugins/docker',
            'plugins/kubernetes',
            'plugins/sql',
            'plugins/authoring',
          ],
        },
      ],
    },
    {
      type: 'category',
      label: 'Flux-Lang',
      collapsed: false,
      items: [
        'language/overview',
        'language/tour',
        'language/playground',
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
      items: [
        'sdk/overview',
        'sdk/sessions',
        'sdk/streaming',
        'sdk/flow-client',
        'sdk/datasources',
        'sdk/agent-lab',
      ],
    },
    {
      type: 'category',
      label: 'Security',
      items: [
        'security/overview',
        'security/plain-terms',
        'security/credentials',
        'security/plugin-sandbox',
        'security/os-sandbox',
        'security/plugin-trust',
        'security/server-auth',
        { type: 'ref', id: 'agent/safety' },
      ],
    },
    {
      type: 'category',
      label: 'Reference',
      items: ['troubleshooting', 'reference/config', 'reference/storage'],
    },
    {
      type: 'category',
      label: 'Direction',
      items: [
        'direction/connector-native-integrations',
        'direction/portable-wasm-runtime',
        'agent/improvement',
      ],
    },
  ],
};

module.exports = sidebars;
