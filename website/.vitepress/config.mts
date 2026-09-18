import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'CrabBoss',
  description: 'Open radio automation software — gapless playout, true-PFL cueing, Icecast streaming.',
  themeConfig: {
    nav: [
      { text: 'Guide', link: '/guide/getting-started' },
      {
        text: 'Roadmap',
        link: 'https://github.com/sonyarianto/crabboss/blob/main/ROADMAP.md',
      },
    ],
    sidebar: [
      {
        text: 'Guide',
        items: [
          { text: 'Getting Started', link: '/guide/getting-started' },
          { text: 'User Manual', link: '/guide/user-manual' },
        ],
      },
      {
        text: 'Project',
        items: [
          {
            text: 'Architecture',
            link: 'https://github.com/sonyarianto/crabboss/blob/main/docs/architecture.md',
          },
          { text: 'GitHub', link: 'https://github.com/sonyarianto/crabboss' },
        ],
      },
    ],
    socialLinks: [{ icon: 'github', link: 'https://github.com/sonyarianto/crabboss' }],
    search: { provider: 'local' },
    footer: {
      message: 'MIT Licensed',
      copyright: 'Copyright © CrabBoss contributors',
    },
  },
})
