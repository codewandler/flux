// @ts-check

const {themes: prismThemes} = require('prism-react-renderer');

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
  // Anchors drift silently when a heading is reworded — flow-client.md was already pointing at a
  // renamed heading in tooling.md. Same reasoning as onBrokenLinks: make it fail, not warn.
  onBrokenAnchors: 'throw',

  // Raster fallback for browsers without SVG-favicon support; per assets/README.md the
  // PNG/ICO derivatives carry the light-mode palette. `headTags` below adds the adaptive
  // SVG after it, which supporting browsers prefer.
  favicon: 'img/favicon.ico',

  headTags: [
    {
      tagName: 'link',
      attributes: {rel: 'icon', type: 'image/svg+xml', href: '/flux/img/favicon.svg'},
    },
    {
      tagName: 'link',
      attributes: {rel: 'apple-touch-icon', sizes: '180x180', href: '/flux/img/apple-touch-icon.png'},
    },
  ],
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'throw',
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
          editUrl: 'https://github.com/codewandler/flux/edit/main/website/',
        },
        blog: {
          path: 'blog',
          routeBasePath: 'blog',
          blogTitle: 'flux blog',
          blogDescription:
            'Tutorials and release walkthroughs for flux — the deterministic agent platform.',
          blogSidebarTitle: 'Recent posts',
          blogSidebarCount: 10,
          showReadingTime: true,
          // A post is a dated claim about how flux behaved at a point in time. Truncating the feed
          // to summaries keeps the index readable; `<!-- truncate -->` marks the cut in each post.
          truncateMarker: /<!--\s*truncate\s*-->/,
          onUntruncatedBlogPosts: 'warn',
          feedOptions: {
            type: ['rss', 'atom'],
            title: 'flux blog',
            description:
              'Tutorials and release walkthroughs for flux — the deterministic agent platform.',
            copyright: `Copyright © ${new Date().getFullYear()} codewandler`,
          },
        },
        theme: {
          customCss: require.resolve('./src/css/custom.css'),
        },
      },
    ],
  ],

  themes: [
    [
      require.resolve('@easyops-cn/docusaurus-search-local'),
      {
        // Offline, index-based search — no external service (Algolia) needed.
        hashed: true,
        indexBlog: true,
        docsRouteBasePath: 'docs',
        highlightSearchTermsOnTargetPage: true,
      },
    ],
  ],

  plugins: [require.resolve('./plugins/llms-txt')],

  themeConfig: {
    image: 'img/social-card.png',
    metadata: [
      {
        name: 'description',
        content:
          'flux is a deterministic agent platform: provider-native typed stages propose literal ' +
          'calls inside an authored Flux-Lang loop; the host freezes effects into action batches ' +
          'and runs them through authorization, approval, and guarded IO.',
      },
      {name: 'keywords', content: 'flux, Flux-Lang, agent platform, Rust agent SDK, deterministic agents'},
    ],
    colorMode: {
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'flux',
      logo: {
        alt: 'flux execution-gate mark',
        src: 'img/flux-mark-light.svg',
        srcDark: 'img/flux-mark-dark.svg',
        height: 26,
      },
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
          to: '/docs/whats-new',
          label: "What's new",
          position: 'left',
        },
        {
          to: '/blog',
          label: 'Blog',
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
            { label: 'Channels', to: '/docs/channels/overview' },
            { label: "What's new", to: '/docs/whats-new' },
            { label: 'Blog', to: '/blog' },
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
      // Both must be set explicitly: with neither, Docusaurus applies its `palenight`
      // default to *both* modes, so code blocks render dark on the light theme.
      theme: prismThemes.github,
      darkTheme: prismThemes.palenight,
      additionalLanguages: ['bash', 'json', 'lua', 'rust', 'toml'],
    },
  },
};

module.exports = config;
