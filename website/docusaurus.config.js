// @ts-check

const config = {
  title: 'flux',
  tagline: 'A deterministic agent platform where the LLM is not the runtime.',
  url: 'https://codewandler.github.io',
  baseUrl: '/flux/',
  organizationName: 'codewandler',
  projectName: 'flux',
  deploymentBranch: 'gh-pages',
  trailingSlash: false,
  onBrokenLinks: 'throw',
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'warn',
    },
  },

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: require.resolve('./sidebars.js'),
          routeBasePath: 'docs',
          editUrl: 'https://github.com/codewandler/flux/tree/main/website/',
        },
        blog: false,
        theme: {
          customCss: require.resolve('./src/css/custom.css'),
        },
      },
    ],
  ],

  themeConfig: {
    navbar: {
      title: 'flux',
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docs',
          position: 'left',
          label: 'Docs',
        },
        {
          to: '/docs/language/overview',
          label: 'Flux-Lang',
          position: 'left',
        },
        {
          href: 'https://github.com/codewandler/flux',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Docs',
          items: [
            { label: 'Getting started', to: '/docs/getting-started' },
            { label: 'Flux-Lang', to: '/docs/language/overview' },
            { label: 'SDK', to: '/docs/sdk/flow-client' },
          ],
        },
        {
          title: 'Project',
          items: [
            { label: 'GitHub', href: 'https://github.com/codewandler/flux' },
          ],
        },
      ],
      copyright: `Copyright (c) ${new Date().getFullYear()} codewandler.`,
    },
    prism: {
      additionalLanguages: ['bash', 'json', 'rust', 'toml'],
    },
  },
};

module.exports = config;
